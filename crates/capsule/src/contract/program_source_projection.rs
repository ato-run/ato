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
//! Neither root a derivation reads is a path a caller may hold: the staging
//! copy is process-private always, and the pinned root is process-private
//! whenever the proof was minted from an archive (the value owns the extraction
//! directory). Withholding the accessors is only half of that — `Debug` and
//! error text disclose a path just as effectively — so
//! [`VerifiedPinnedSourceMaterialization`] redacts its `Debug`, the staged and
//! projected values have none, and every message leaving this module names
//! paths RELATIVE to the root they live under (`relativize_roots`). The one
//! absolute path that stays is the archive passed to
//! [`VerifiedPinnedSourceMaterialization::from_source_archive`] — the caller's
//! own input, which it already holds.
//!
//! There is exactly ONE public minting path:
//! [`VerifiedPinnedSourceMaterialization::from_source_archive`], which extracts
//! a content-addressed `.tar.zst`
//! ([`materialize_source_archive`](crate::foundation::blob::materialize_source_archive)'s
//! output) into a process-private directory the returned value owns, so the
//! proof holds *by construction*. No API lets a caller self-attest a directory
//! it already has: the shape-checking constructor is `#[cfg(test)] pub(crate)`,
//! reachable only from this crate's own unit tests.
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

use std::fmt;
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

/// How a caller-visible message names the tree root itself when the offending
/// path IS the root. Both roots a derivation reads are process-private — the
/// staging copy always, the pinned root whenever the proof was minted from an
/// archive — so no message may carry either absolute path.
const SOURCE_ROOT_LABEL: &str = "<source root>";

// ─────────────────────────────────────────────────────────────────────────────
// Proof-carrying input boundary
// ─────────────────────────────────────────────────────────────────────────────

/// A proof-carrying wrapper over a filesystem root that IS a **pinned source
/// materialization** (ADR-014 §1): an immutable archive / `source_materialize`
/// output, extracted and validated.
///
/// It cannot be minted from a bare `PathBuf`: the fields are private and there
/// is no public constructor — no `new`, no `From<PathBuf>`, no `TryFrom<&Path>`,
/// no `Deserialize`. Phase 0 has exactly ONE public minting path,
/// [`VerifiedPinnedSourceMaterialization::from_source_archive`], which mints the
/// proof **by construction** from a content-addressed source archive: the
/// archive bytes are immutable and named by their own hash, and the extraction
/// target is a fresh process-private directory the returned value owns, so
/// nothing about the resulting root is asserted. There is deliberately no
/// public constructor that takes a directory and believes the caller: such an
/// API would move the guarantee out of the type and into a convention, and the
/// claim this type carries — "these bytes came from a content-addressed
/// materializer" — is exactly what a caller assertion cannot establish.
///
/// The shape-checking constructor `for_test` exists only under `#[cfg(test)]`
/// and only as `pub(crate)`: it is reachable from this crate's unit tests, and
/// from nothing else — not from this crate's `tests/` integration crates, and
/// not from any downstream crate. The compile-fail proof below is that
/// statement executed by the compiler.
///
/// The staging copy taken during derivation mints the same
/// by-construction proof internally (a process-private directory no other
/// process holds a path to), which is why every read after the admissibility
/// gate is provably from a pinned tree.
///
/// # The materializer seam
///
/// A source resolver / CAS materializer that yields an already-**extracted**
/// pinned tree would not need to be re-archived to be admitted: it would get
/// its own crate-internal minting seam, taking that materializer's own
/// capability type (a value only the materializer can produce) rather than a
/// `&Path`. No such producer exists in this repo today —
/// [`materialize_source_archive`](crate::foundation::blob::materialize_source_archive)
/// emits the archive and its A1v2 hashes but never extracts — so no such seam
/// is declared here. Declaring one before its producer exists would be an
/// unproven capability type: a second self-attestation wearing a different
/// name.
///
/// The wrapper mirrors
/// [`VerifiedExecutionId`](crate::execution_contract::VerifiedExecutionId): the
/// value is minted by the operation that establishes the property, never by a
/// caller stating it. What every minting path enforces, fail closed, is the
/// shape a pinned materialization must have — an existing directory with no
/// root-level `.git`.
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
/// a bare path cannot stand in for the proof at the derivation entrypoint:
///
/// ```compile_fail
/// use std::path::Path;
/// use capsule::capsule_program_contract::derive_capsule_program_contract;
///
/// // A &Path is not a &VerifiedPinnedSourceMaterialization: type error.
/// let _ = derive_capsule_program_contract(Path::new("/tmp/pinned"));
/// ```
///
/// and — the guarantee this type actually carries — a consumer of this crate
/// cannot self-attest an arbitrary directory into the proof. The only public
/// mint is `from_source_archive`; the shape-checking constructor is
/// `#[cfg(test)] pub(crate)`, so outside this crate's unit tests it does not
/// exist. This doctest compiles as a downstream crate, which is why it proves
/// the closure rather than restating it:
///
/// ```compile_fail
/// use std::path::Path;
/// use capsule::program_source_projection::VerifiedPinnedSourceMaterialization;
///
/// // `for_test` is `#[cfg(test)] pub(crate)`: no such associated function is
/// // visible here, so there is no path from a directory to the proof.
/// let _proof = VerifiedPinnedSourceMaterialization::for_test(Path::new("/tmp/pinned"));
/// ```
///
/// Holding the proof does not hand out a writable path to the pinned tree
/// either. Process-private is not the same as unwritable: the value's own
/// holder runs in that process, so an accessor returning the root would let it
/// rewrite `capsule.toml` — or hand the `PathBuf` to a thread — between the
/// A1v2 admissibility pass and the staging copy, which is exactly the window
/// staging exists to close. `root` is `pub(crate)`, so no consumer of this
/// crate can obtain the path:
///
/// ```compile_fail
/// use std::path::Path;
/// use capsule::program_source_projection::VerifiedPinnedSourceMaterialization;
///
/// // `root` is `pub(crate)`: not visible here, so a downstream holder of the
/// // proof has no way to reach — and therefore no way to write — the tree the
/// // proof attests to.
/// fn pinned_root(proof: &VerifiedPinnedSourceMaterialization) -> &Path {
///     proof.root()
/// }
/// ```
///
/// Method visibility alone does not close that boundary: an observability
/// surface hands out the same path without an accessor. A derived `Debug`
/// prints private fields, so it would disclose `root` — and the `TempDir`
/// guard's own `Debug` would disclose it a second time; the hand-written
/// [`fmt::Debug`] impl below discloses neither. Error text is the other such
/// surface, which is why nothing this module returns names an absolute path
/// inside a pinned or staged tree (see [`relativize_roots`]).
#[derive(Clone)]
pub struct VerifiedPinnedSourceMaterialization {
    root: PathBuf,
    /// Ownership guard for a root this value extracted itself
    /// ([`Self::from_source_archive`]): the extracted directory must outlive
    /// every handle to the proof, so the `TempDir` is kept behind an `Arc` that
    /// `Clone` shares — the last surviving handle removes the directory. The
    /// test-only `for_test` path leaves this `None`: that root belongs to the
    /// test and this value must never delete it.
    ///
    /// Never read — the field exists for its `Drop`, which is precisely the
    /// point: the extracted tree must not disappear while a handle to the proof
    /// is alive, and must disappear when the last one is gone.
    #[allow(dead_code)]
    owned_root: Option<Arc<TempDir>>,
}

/// Redacted, not derived: `#[derive(Debug)]` prints private fields, so it would
/// hand any holder — including a downstream one — the writable root that
/// [`VerifiedPinnedSourceMaterialization::root`] withholds, and the `TempDir`
/// guard would print it again. Nothing observable here identifies the tree;
/// `finish_non_exhaustive` says so rather than implying the struct has one
/// field.
impl fmt::Debug for VerifiedPinnedSourceMaterialization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedPinnedSourceMaterialization")
            .field("root", &"<redacted>")
            .finish_non_exhaustive()
    }
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
    ///   the caller.
    ///
    /// This is the type's only public constructor, and the cost of that is
    /// deliberate: a caller holding an already-extracted tree must archive it
    /// (`materialize_source_archive`) to be admitted. Paying one archive+extract
    /// round trip is the price of the proof being earned; see the type's
    /// "materializer seam" note for the shape the cheaper path must take when a
    /// producer that yields an extracted pinned tree exists.
    ///
    /// The extractor is the trust boundary for hostile archive bytes; see
    /// `extract_source_archive` for the entry-kind / path / cap whitelist it
    /// enforces before a single byte is written. After extraction the root goes
    /// through `ensure_pinned_materialization_shape`, the same fail-closed shape
    /// gate re-run at staging time, so the invariant is one function, not two.
    ///
    /// The extraction root never reaches a rejection message: it is this
    /// value's process-private directory, so every path an error names is
    /// relative to it (`archive_tar_zst` itself stays absolute — it is the
    /// caller's own input).
    pub fn from_source_archive(archive_tar_zst: &Path) -> Result<Self, CapsuleProgramError> {
        let extracted = TempDir::new().map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "failed to create the source-archive extraction directory: {source}"
            ))
        })?;
        let root = extracted.path().to_path_buf();
        // A failure here drops `extracted`, which removes any partially written
        // tree: a rejected archive leaves nothing behind.
        extract_source_archive(archive_tar_zst, &root)
            .and_then(|()| ensure_pinned_materialization_shape(&root))
            .map_err(|error| relativize_roots(error, &[&root]))?;
        Ok(Self {
            root,
            owned_root: Some(Arc::new(extracted)),
        })
    }

    /// Wrap `root` after the shape gate alone, WITHOUT establishing that it came
    /// from a content-addressed materializer — i.e. the proof is asserted, not
    /// earned.
    ///
    /// It is `#[cfg(test)] pub(crate)` because that is the only context in which
    /// asserting the claim is legitimate: a unit test constructs the tree it
    /// then attests to, so the assertion is discharged by the test itself. It is
    /// invisible to this crate's `tests/` integration crates (separate crates,
    /// public API only) and to every downstream crate, which is what keeps
    /// [`Self::from_source_archive`] the only mint any caller can reach.
    ///
    /// It runs the same shape gate the earned path runs, so a test tree that is
    /// provably NOT a pinned materialization is refused here too:
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
    #[cfg(test)]
    pub(crate) fn for_test(root: &Path) -> Result<Self, CapsuleProgramError> {
        ensure_pinned_materialization_shape(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            owned_root: None,
        })
    }

    /// The pinned root. Reading it directly re-opens the mutation window the
    /// staging copy exists to close, so it is `pub(crate)`: the only caller is
    /// [`StagedCapsuleSource::stage`], which reads it once to run the A1v2 gate
    /// and take the copy. No consumer of this crate can reach it — a holder of
    /// the proof must derive from [`StagedCapsuleSource`], which owns a tree
    /// nobody else has a path to. The compile-fail proof on the type is that
    /// statement executed by the compiler.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// Fail-closed shape checks for a pinned materialization root. Cheap enough
/// (two `symlink_metadata` calls) to run both when the proof is minted and
/// again when it is used, so a tree that changed in between still fails closed.
///
/// `root` is never named in a message: an archive-minted proof's root is the
/// process-private extraction directory, so printing it would disclose a
/// writable path and tell the caller nothing it can act on.
fn ensure_pinned_materialization_shape(root: &Path) -> Result<(), CapsuleProgramError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            return Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{SOURCE_ROOT_LABEL} is not a directory"
            )));
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{SOURCE_ROOT_LABEL} does not exist"
            )));
        }
        Err(source) => {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "failed to inspect {SOURCE_ROOT_LABEL}: {source}"
            )));
        }
    }

    let git = root.join(GIT_METADATA_DIR_NAME);
    let git_state = lexical_entry_state(&git).map_err(|source| {
        CapsuleProgramError::SourceProjection(format!(
            "failed to inspect {GIT_METADATA_DIR_NAME}: {source}"
        ))
    })?;
    match git_state {
        LexicalEntryState::Absent => Ok(()),
        LexicalEntryState::PresentRegularFile | LexicalEntryState::PresentInvalidNode(_) => {
            Err(CapsuleProgramError::NotPinnedMaterialization(format!(
                "{SOURCE_ROOT_LABEL} contains a root-level {GIT_METADATA_DIR_NAME}: a Git \
                 checkout is a working tree, and ADR-014 §1 admits only a pinned source \
                 materialization (immutable archive / source_materialize output) in Phase 0"
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
///
/// The fields are absolute paths under the root they were resolved from, and
/// the derived `Debug` prints them — which is safe only because no value a
/// consumer of this crate can obtain carries a process-private root. The one
/// resolved over the staging copy lives inside [`StagedCapsuleSource`], which
/// exposes no accessor for it; every other value comes from
/// [`resolve_capsule_control_files`] over a root the caller supplied and
/// already holds.
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
/// original tree. That is also why every rejection here is relativized against
/// `selected_root`: in the derivation flow the root is the process-private
/// staging copy, and a caller shown `<staging>/capsule.toml` would be shown a
/// path it can write to and cannot act on.
pub fn resolve_capsule_control_files(
    selected_root: &Path,
) -> Result<CapsuleControlFiles, CapsuleProgramError> {
    resolve_control_files_in(selected_root)
        .map_err(|error| relativize_roots(error, &[selected_root]))
}

fn resolve_control_files_in(
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
///
/// Deliberately has no `Debug`: both roots it holds are process-private, and a
/// derived one would print them straight out of the private fields.
pub struct StagedCapsuleSource {
    /// The pinned root this copy was taken from — kept only so
    /// [`Self::relativize`] can strip it out of a message that some frozen
    /// layer below built from an absolute path.
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
        // A1's own message embeds the offending path absolutely; relativizing
        // below is what keeps the pinned root out of it.
        materialized_source_tree_hash(origin)
            .map_err(|source| {
                CapsuleProgramError::SourceProjection(format!(
                    "A1v2 admissibility rejected the pinned source tree: {source}"
                ))
            })
            .map_err(|error| relativize_roots(error, &[origin]))?;

        let staging = TempDir::new().map_err(|source| {
            CapsuleProgramError::SourceProjection(format!(
                "failed to create the staging directory: {source}"
            ))
        })?;
        copy_tree(origin, staging.path())
            .map_err(|error| relativize_roots(error, &[origin, staging.path()]))?;

        // Steps 2–3, resolved in the copy: from here on the original tree is
        // never read again, so nothing an outside process does to it can reach
        // the manifest intent or the digest. `resolve_capsule_control_files`
        // already relativizes against the root it was given — the staging copy.
        let control_files = resolve_capsule_control_files(staging.path())?;

        Ok(Self {
            origin: origin.to_path_buf(),
            staging,
            control_files,
        })
    }

    /// The staging root. Manifest loading and every `SourceExistingPath`
    /// existence check resolve against this path.
    ///
    /// `pub(crate)` for the same reason as
    /// [`VerifiedPinnedSourceMaterialization::root`]: the staging tree is the
    /// one tree the digest is taken over, so a caller holding its path could
    /// write into it between the manifest read and
    /// [`ProjectedCapsuleSource::source_contract`]. The only caller is
    /// `derive_capsule_program_contract`, which is in this crate.
    pub(crate) fn root(&self) -> &Path {
        self.staging.path()
    }

    /// `<staging>/capsule.toml` — the only manifest a derivation may read.
    ///
    /// `pub(crate)` for the same reason as [`Self::root`]: this is an absolute
    /// path into the staging tree, so handing it to a consumer of this crate
    /// would hand it a file it can rewrite between the manifest read and the
    /// digest.
    pub(crate) fn manifest_path(&self) -> &Path {
        &self.control_files.manifest
    }

    /// Strips both process-private roots out of `error`, leaving every path it
    /// names relative to the tree it lives in. Applied by
    /// `derive_capsule_program_contract` to the layers that read the staging
    /// tree through a path — manifest loading and the strict adapter's
    /// `SourceExistingPath` checks — because those build their messages from
    /// the absolute path they were handed.
    pub(crate) fn relativize(&self, error: CapsuleProgramError) -> CapsuleProgramError {
        relativize_roots(error, &[self.staging.path(), &self.origin])
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
                relativize_roots(
                    projection_io("exclude control file", path, source),
                    &[staging.path(), &origin],
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
///
/// Deliberately has no `Debug`, for the same reason as [`StagedCapsuleSource`]:
/// the only field is the process-private staging directory.
pub struct ProjectedCapsuleSource {
    staging: TempDir,
}

impl ProjectedCapsuleSource {
    /// The projected file set: every regular file in the projected tree as a
    /// `/`-joined path relative to the projected root, sorted lexicographically.
    /// This is exactly the file set [`Self::source_contract`]'s digest covers,
    /// and it is what a cross-implementation vector harness needs from the
    /// projection.
    ///
    /// It returns the paths rather than the root deliberately. The root is a
    /// live directory: handing it out would let the caller add, remove, or
    /// rewrite a file between this call and `source_contract`, so the enumerated
    /// set and the digest could describe different trees — the two values the
    /// vectors pin *together*. Names are the projection's own bytes, so there is
    /// no path back to the tree.
    ///
    /// A non-UTF-8 name, a symlink, or a special node here is not transliterated
    /// or skipped: A1v2 admissibility and `copy_tree` already rejected all three
    /// before staging, so encountering one means the tree changed afterwards and
    /// this fails closed.
    pub fn projected_file_paths(&self) -> Result<Vec<String>, CapsuleProgramError> {
        let mut paths = Vec::new();
        collect_projected_files(self.staging.path(), "", &mut paths)
            .map_err(|error| relativize_roots(error, &[self.staging.path()]))?;
        paths.sort();
        Ok(paths)
    }

    /// Step 6: the frozen A1 digest over the projected root.
    pub fn source_contract(&self) -> Result<ProgramSourceContract, CapsuleProgramError> {
        let blob_hash = materialized_source_tree_hash(self.staging.path())
            .map_err(|source| {
                CapsuleProgramError::SourceProjection(format!(
                    "failed to hash the projected source tree: {source}"
                ))
            })
            .map_err(|error| relativize_roots(error, &[self.staging.path()]))?;
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

/// Appends every regular file under `dir` to `out` as a `/`-joined path
/// relative to the projected root, `prefix` being that relative path of `dir`
/// itself (empty at the root). Order is imposed by the caller's sort, not by
/// `read_dir`, whose order is unspecified.
fn collect_projected_files(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), CapsuleProgramError> {
    let entries =
        fs::read_dir(dir).map_err(|source| projection_io("read directory", dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| projection_io("read directory", dir, source))?;
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "non-UTF-8 path component at {} in the projected tree (tree changed \
                 after the admissibility pass)",
                path.display(),
            )));
        };
        let relative = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        let file_type = fs::symlink_metadata(&path)
            .map_err(|source| projection_io("inspect entry", &path, source))?
            .file_type();
        if file_type.is_dir() {
            collect_projected_files(&path, &relative, out)?;
        } else if file_type.is_file() {
            out.push(relative);
        } else {
            return Err(CapsuleProgramError::SourceProjection(format!(
                "unexpected {} at {} in the projected tree (tree changed after the \
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

/// Rewrites every absolute path under one of `roots` into a path relative to
/// that root, so no caller-visible message names a directory the caller could
/// write to.
///
/// This replaces the earlier "rewrite the staging path back to the pinned root"
/// mapping, which was not enough: for an archive-minted proof the pinned root
/// IS a process-private extraction directory this crate owns, so attributing a
/// message to it discloses exactly the path
/// [`VerifiedPinnedSourceMaterialization::root`] withholds — and names
/// something the caller cannot act on anyway. A path relative to the root is
/// what the caller's own input (archive entry, source tree) is expressed in.
///
/// Textual rather than structural because the offending path is usually
/// embedded by a layer below this one — A1v2 admissibility names the entry it
/// rejected, `load_manifest` names the file it failed to parse — and those
/// messages are frozen `String`s by the time they arrive. Longest-form first:
/// `<root>/` collapses to the relative remainder, and only a bare `<root>` left
/// over becomes [`SOURCE_ROOT_LABEL`].
///
/// Variants carrying no path (ids, schema, field names) are returned untouched.
fn relativize_roots(error: CapsuleProgramError, roots: &[&Path]) -> CapsuleProgramError {
    let rewrite = |message: String| -> String {
        roots.iter().fold(message, |message, root| {
            // Match the rendering the message was BUILT with. Every path that
            // reaches an error here arrives through `Path::display()`, which is
            // lossy, so a `to_str()` match would silently skip redaction for a
            // non-UTF-8 root while `display()` still wrote a recognizable form
            // of it into the message — and the caller who chose that root's
            // parent (e.g. via TMPDIR) only needs the random basename to
            // reconstruct a writable path.
            let rendered = root.display().to_string();
            message
                .replace(&format!("{rendered}{}", std::path::MAIN_SEPARATOR), "")
                .replace(&rendered, SOURCE_ROOT_LABEL)
        })
    };
    match error {
        CapsuleProgramError::SourceProjection(message) => {
            CapsuleProgramError::SourceProjection(rewrite(message))
        }
        CapsuleProgramError::ManifestLoad(message) => {
            CapsuleProgramError::ManifestLoad(rewrite(message))
        }
        CapsuleProgramError::NotPinnedMaterialization(message) => {
            CapsuleProgramError::NotPinnedMaterialization(rewrite(message))
        }
        CapsuleProgramError::ManifestInput(message) => {
            CapsuleProgramError::ManifestInput(rewrite(message))
        }
        CapsuleProgramError::Canonicalization(message) => {
            CapsuleProgramError::Canonicalization(rewrite(message))
        }
        CapsuleProgramError::InvalidValue { field, reason } => CapsuleProgramError::InvalidValue {
            field,
            reason: rewrite(reason),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// The test-only mint. Legitimate here because each test builds the tree it
    /// attests to; `for_test` is unreachable from any other crate, so this
    /// shortcut cannot leak into a caller.
    fn pinned(root: &Path) -> VerifiedPinnedSourceMaterialization {
        VerifiedPinnedSourceMaterialization::for_test(root).expect("pinned materialization")
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
        // The message names the two lock spellings relative to the root, and
        // neither the staging copy nor the pinned root itself.
        assert!(
            !message.contains(&tmp.path().display().to_string()),
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
        // The manifest is named relative to the root, never as the absolute
        // path of the staging copy or of the pinned tree behind it.
        assert!(
            !message.contains(&tmp.path().display().to_string()),
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

        let err = VerifiedPinnedSourceMaterialization::for_test(checkout.path()).unwrap_err();
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
            VerifiedPinnedSourceMaterialization::for_test(gitfile.path()),
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
            VerifiedPinnedSourceMaterialization::for_test(&tmp.path().join("absent")),
            Err(CapsuleProgramError::NotPinnedMaterialization(_))
        ));

        write_file(tmp.path(), "file.txt", b"x\n");
        assert!(matches!(
            VerifiedPinnedSourceMaterialization::for_test(&tmp.path().join("file.txt")),
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

    /// THE load-bearing test: the earned mint and the test-only mint must
    /// produce the same program source digest over the same tree. The archive
    /// round trip is an isolation mechanism, not a projection change — if the
    /// two ever diverged, `from_source_archive` would be minting a *different*
    /// program identity, and the recorded source vectors (which now go through
    /// the archive) would silently be pinning something else.
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
            VerifiedPinnedSourceMaterialization::for_test(tree.path()),
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

    // ── observability boundary (Debug + error text) ──────────────────────

    /// `Debug` is an accessor by another name: a derived one prints the private
    /// `root` field, and the `TempDir` guard's own `Debug` prints it a second
    /// time, so narrowing [`VerifiedPinnedSourceMaterialization::root`] to
    /// `pub(crate)` alone would leave the writable path in reach of every
    /// holder. Asserted for both mints, since only the archive-minted one owns
    /// its root but both must redact.
    #[test]
    fn debug_discloses_neither_the_pinned_root_nor_the_ownership_guard() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        let (_archive_dir, archive) = materialize(tree.path());

        let earned = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap();
        let earned_root = earned.root().display().to_string();
        let rendered = format!("{earned:?}");
        assert!(
            !rendered.contains(&earned_root),
            "the extracted root must not be observable: {rendered}"
        );
        // The `TempDir` guard prints the same path through its own `Debug`, so
        // the whole field must be withheld, not just renamed.
        assert!(!rendered.contains("TempDir"), "{rendered}");
        assert!(!rendered.contains("owned_root"), "{rendered}");
        // Non-vacuous: the value still identifies itself.
        assert!(
            rendered.contains("VerifiedPinnedSourceMaterialization"),
            "{rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let asserted = pinned(tree.path());
        let rendered = format!("{asserted:?}");
        assert!(
            !rendered.contains(&tree.path().display().to_string()),
            "{rendered}"
        );
    }

    /// Redacting `Debug` must not change what the value IS: `Clone`/`PartialEq`
    /// still compare the pinned root, which is what
    /// `archive_minted_root_outlives_the_call_and_dies_with_the_last_handle`
    /// relies on.
    #[test]
    fn redacted_debug_leaves_clone_and_equality_unchanged() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        let proof = pinned(tree.path());

        assert_eq!(proof, proof.clone());

        let other = TempDir::new().unwrap();
        write_base_tree(other.path());
        assert_ne!(proof, pinned(other.path()));
    }

    /// A missing manifest, derived from an ARCHIVE-minted proof: the root the
    /// message would have named is the extraction directory this crate owns, so
    /// naming it disclosed a writable path AND told the caller nothing.
    #[test]
    fn missing_manifest_from_an_archive_minted_proof_names_a_relative_path() {
        let tree = TempDir::new().unwrap();
        write_file(tree.path(), "src/main.py", b"print('hi')\n");
        let (_archive_dir, archive) = materialize(tree.path());
        let proof = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap();
        let private_root = proof.root().display().to_string();

        let err = project_program_source(&proof).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(
            !message.contains(&private_root),
            "the process-private extraction root must not reach the message: {message}"
        );
        // Non-vacuous: redaction that dropped the path entirely would leave the
        // message unactionable and would still satisfy the assertion above.
        assert!(
            message.contains("required manifest capsule.toml does not exist"),
            "{message}"
        );
    }

    /// Lock coexistence, same proof shape: the rejection names both spellings
    /// relative to the root.
    #[test]
    fn lock_coexistence_from_an_archive_minted_proof_names_relative_paths() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        write_file(tree.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);
        write_file(
            tree.path(),
            DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME,
            LOCK_BODY,
        );
        let (_archive_dir, archive) = materialize(tree.path());
        let proof = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap();
        let private_root = proof.root().display().to_string();

        let err = project_program_source(&proof).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &err else {
            panic!("expected SourceProjection, got {err:?}");
        };
        assert!(!message.contains(&private_root), "{message}");
        assert!(message.contains(CAPSULE_LOCK_FILE_NAME), "{message}");
        assert!(
            message.contains(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME),
            "{message}"
        );
    }

    /// An archive whose extracted tree carries a root-level `.git` is rejected
    /// inside `from_source_archive`, where the only root in scope is the
    /// extraction directory. The archive path itself is the caller's own input
    /// and stays absolute — that is the diagnostic the caller can act on.
    #[test]
    fn archive_rejections_name_the_archive_but_never_the_extraction_root() {
        let tree = TempDir::new().unwrap();
        write_base_tree(tree.path());
        write_file(tree.path(), ".git/HEAD", b"ref: refs/heads/main\n");
        let (_archive_dir, archive) = materialize(tree.path());

        let err = VerifiedPinnedSourceMaterialization::from_source_archive(&archive).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(message.contains(GIT_METADATA_DIR_NAME), "{message}");
        // The extraction root is gone with the rejected proof, so it cannot be
        // compared directly; no temp-dir path may appear at all.
        assert!(
            !message.contains(std::env::temp_dir().to_str().unwrap()),
            "{message}"
        );

        // The caller's own archive path is kept, and only that one.
        let missing = archive.parent().unwrap().join("absent.tar.zst");
        let err = VerifiedPinnedSourceMaterialization::from_source_archive(&missing).unwrap_err();
        let CapsuleProgramError::NotPinnedMaterialization(message) = &err else {
            panic!("expected NotPinnedMaterialization, got {err:?}");
        };
        assert!(
            message.contains(&missing.display().to_string()),
            "the archive the caller passed stays absolute: {message}"
        );
    }

    /// Control-file exclusion is by removal from the staging copy; the file set
    /// that reaches the digest is exactly "everything but the resolved control
    /// files".
    ///
    /// Asserted through the public [`ProjectedCapsuleSource::projected_file_paths`]
    /// rather than by joining paths onto a projected root, because that set —
    /// not a directory handle — is what the projection now exposes. Equality
    /// against the full expected vector also pins what per-path `exists()`
    /// checks could not: that nothing *else* survived, and that nested
    /// control-file NAMES are ordinary source.
    #[test]
    fn projected_tree_contains_everything_but_the_control_files() {
        let tmp = TempDir::new().unwrap();
        write_base_tree(tmp.path());
        write_file(tmp.path(), CAPSULE_LOCK_FILE_NAME, LOCK_BODY);

        let projected = StagedCapsuleSource::stage(&pinned(tmp.path()))
            .unwrap()
            .into_projected()
            .unwrap();

        assert_eq!(
            projected.projected_file_paths().unwrap(),
            vec![
                "examples/capsule.toml".to_string(),
                "fixtures/ato.lock.json".to_string(),
                "src/main.py".to_string(),
            ],
        );
    }

    /// A non-UTF-8 root must still be redacted. Messages are built with
    /// `Path::display()` (lossy), so matching on `to_str()` would skip
    /// redaction entirely for such a root while `display()` had already
    /// written a recognizable form of it into the message — and a caller who
    /// chose the parent (e.g. by setting `TMPDIR`) needs only the random
    /// basename to reconstruct a writable path and defeat the boundary.
    #[cfg(unix)]
    #[test]
    fn relativization_redacts_a_non_utf8_root() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = PathBuf::from(OsString::from_vec(
            b"/tmp/non-utf8-\xff/private-root".to_vec(),
        ));
        assert!(root.to_str().is_none(), "fixture must not be valid UTF-8");

        let redacted = relativize_roots(
            CapsuleProgramError::SourceProjection(format!(
                "required manifest {} does not exist",
                root.join("capsule.toml").display()
            )),
            &[&root],
        )
        .to_string();

        assert!(
            !redacted.contains("private-root"),
            "non-UTF-8 root leaked: {redacted}"
        );
        // Non-vacuous: the message must still be actionable.
        assert!(redacted.contains("capsule.toml"), "{redacted}");
        assert!(redacted.contains("does not exist"), "{redacted}");
    }
}
