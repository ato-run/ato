//! The v3 outer reader: format dispatch, and staging the four outer members.
//!
//! # Dispatch
//!
//! RFC §"v2 / v3 dispatch": `index.json` at the outer root is the *only* signal.
//! Present ⇒ the bundle MUST validate as v3, and an invalid index or signature is
//! a rejection, never a fallback. Absent ⇒ hand the bytes to the existing v2
//! reader, whose validity rules are its own business.
//!
//! This module deliberately does **not** enumerate v2's outer shape. An earlier
//! revision of the RFC tried to, got it wrong (`packers/capsule.rs` also emits an
//! outer payload-manifest member and an outer README), and would have made a v3
//! reader reject bundles the current v2 writer legitimately produces. A second,
//! drifting copy of v2's shape is worse than no copy: [`BundleFormat::V2Legacy`]
//! is a hand-off, not a verdict.
//!
//! The v2 reader itself lives in `crates/cli/src/utils/archive.rs`, one crate up
//! the dependency graph, so this crate signals the hand-off with
//! [`BundleFormat::V2Legacy`] / [`CapsuleImportError::NotV3Bundle`] rather than
//! calling it.
//!
//! # The outer allowlist
//!
//! Exactly four regular-file members, matched on **raw path bytes**:
//!
//! ```text
//! capsule.toml  index.json  signature.json  source.tar.zst
//! ```
//!
//! Matching raw bytes for equality is what makes the alias cases fall out
//! automatically rather than needing their own sanitizer: `./index.json`,
//! `/index.json`, `../index.json`, `.\index.json`, and `index.json\0x` are all
//! simply not equal to `index.json`. Entry *kind* is checked before the name, so
//! a symlink named `index.json` is rejected as a symlink and never gets to be a
//! member at all.
//!
//! Member order in the TAR is not a validity condition — the writer emits a
//! deterministic order, but a reader that demanded one would be asserting
//! something the format does not fix. The exact-four allowlist is the condition.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tar::EntryType;
use tempfile::TempDir;

use super::CapsuleImportError;
use super::index::Sha256Digest;
use super::policy::CapsuleImportPolicy;

/// The outer member carrying `ato.capsule-index/v1`.
pub const INDEX_MEMBER_PATH: &str = "index.json";

/// The outer member carrying `ato.capsule-index-signature/v1`.
pub const SIGNATURE_MEMBER_PATH: &str = "signature.json";

/// The complete outer allowlist, in ascending byte order.
pub const V3_OUTER_MEMBER_PATHS: [&str; 4] = [
    super::index::MANIFEST_MEMBER_PATH,
    INDEX_MEMBER_PATH,
    SIGNATURE_MEMBER_PATH,
    super::index::SOURCE_MEMBER_PATH,
];

/// Which container revision an archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    /// A root `index.json` is present: validate as v3 or reject.
    V3,
    /// No root `index.json`: hand off to the existing v2 reader, unmodified.
    V2Legacy,
}

/// Classify an archive by the single dispatch signal.
///
/// The reader is left positioned wherever the scan finished; callers rewind.
///
/// # Errors
///
/// [`CapsuleImportError::CapsuleInvalid`] if the outer TAR stream cannot be read
/// at all — a byte soup is neither v2 nor v3.
pub fn classify_bundle_format<R: Read>(reader: R) -> Result<BundleFormat, CapsuleImportError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|source| {
        CapsuleImportError::invalid(format!("outer TAR stream is unreadable: {source}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| {
            CapsuleImportError::invalid(format!("outer TAR entry is unreadable: {source}"))
        })?;
        if entry.path_bytes().as_ref() == INDEX_MEMBER_PATH.as_bytes() {
            return Ok(BundleFormat::V3);
        }
    }
    Ok(BundleFormat::V2Legacy)
}

/// One staged outer member: its bytes on disk, plus what they actually measure.
#[derive(Debug)]
pub(crate) struct StagedMember {
    path: PathBuf,
    digest: Sha256Digest,
    size: u64,
}

impl StagedMember {
    pub(crate) fn digest(&self) -> Sha256Digest {
        self.digest
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn read_bytes(&self) -> Result<Vec<u8>, CapsuleImportError> {
        std::fs::read(&self.path)
            .map_err(|source| CapsuleImportError::io("read a staged outer member", source))
    }
}

/// The four outer members, staged into a process-private directory.
///
/// Owning the [`TempDir`] is what makes cleanup unconditional: every failure
/// path after this value is constructed drops it, so a rejected bundle leaves
/// nothing behind.
#[derive(Debug)]
pub(crate) struct StagedOuterMembers {
    _staging: TempDir,
    members: BTreeMap<&'static str, StagedMember>,
    /// Bytes charged against the policy so far, carried forward so the later
    /// derivation continues one running total rather than starting a fresh,
    /// unrelated budget for the same import.
    staged_total: u64,
}

impl StagedOuterMembers {
    /// The running policy charge this staging already accrued.
    pub(crate) fn staged_total(&self) -> u64 {
        self.staged_total
    }

    pub(crate) fn member(&self, path: &str) -> &StagedMember {
        self.members
            .get(path)
            .expect("the exact-four allowlist guarantees every member is staged")
    }

    /// The staging root, so a test can prove it is gone after a failure.
    ///
    /// `#[cfg(test)]` because the whole point of owning the [`TempDir`] is that
    /// no consumer holds a path into it.
    #[cfg(test)]
    pub(crate) fn staging_root(&self) -> &Path {
        self._staging.path()
    }
}

/// Read a v3 outer archive, staging exactly the four allowlisted members.
///
/// Bytes are streamed into temp files while hashing, so nothing is ever
/// pre-allocated from a declared size and the resource policy is charged
/// incrementally as bytes land.
///
/// # Errors
///
/// [`CapsuleImportError::NotV3Bundle`] when there is no root `index.json`;
/// [`CapsuleImportError::CapsuleInvalid`] for any allowlist, alias, entry-kind,
/// or duplication violation; the policy's own categories when a budget is hit.
pub(crate) fn stage_v3_outer_members<R: Read>(
    reader: R,
    policy: &CapsuleImportPolicy,
) -> Result<StagedOuterMembers, CapsuleImportError> {
    let staging = TempDir::new()
        .map_err(|source| CapsuleImportError::io("create the outer staging directory", source))?;
    let mut members: BTreeMap<&'static str, StagedMember> = BTreeMap::new();
    let mut staged_total: u64 = 0;

    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|source| {
        CapsuleImportError::invalid(format!("outer TAR stream is unreadable: {source}"))
    })?;

    for entry in entries {
        let mut entry = entry.map_err(|source| {
            CapsuleImportError::invalid(format!("outer TAR entry is unreadable: {source}"))
        })?;

        let raw_path = entry.path_bytes().into_owned();
        let displayed = String::from_utf8_lossy(&raw_path).into_owned();

        // Entry KIND first, before the name is even considered: a symlink,
        // hardlink, device, FIFO, or directory named `index.json` must be
        // rejected as that node type, not silently treated as the member whose
        // name it borrowed.
        let entry_type = entry.header().entry_type();
        if entry_type != EntryType::Regular {
            return Err(CapsuleImportError::invalid(format!(
                "outer member {displayed:?} has entry type {entry_type:?}; a v3 bundle carries \
                 only regular files"
            )));
        }
        if entry.link_name_bytes().is_some() {
            return Err(CapsuleImportError::invalid(format!(
                "outer member {displayed:?} carries a link name"
            )));
        }

        // A TAR name field is NUL-terminated inside a fixed 100 bytes, so
        // `index.json\0junk` reads as `index.json` here — but a reader that
        // treated the field as a fixed-width string would see something else.
        // Two readers disagreeing about a member's name is exactly the ambiguity
        // this format cannot afford, so trailing bytes after the terminator are
        // a rejection rather than something to ignore.
        reject_trailing_bytes_in_name_field(entry.header().as_bytes(), &displayed)?;

        // Raw-byte equality against the allowlist. Every alias form — `./x`,
        // `/x`, `../x`, `.\x`, an embedded NUL, a trailing slash — fails this
        // comparison by construction.
        let Some(name) = V3_OUTER_MEMBER_PATHS
            .iter()
            .copied()
            .find(|candidate| candidate.as_bytes() == raw_path.as_slice())
        else {
            return Err(CapsuleImportError::invalid(format!(
                "outer member {displayed:?} is not one of the four v3 members \
                 ({}); no extra entries and no path aliases are admitted",
                V3_OUTER_MEMBER_PATHS.join(", ")
            )));
        };

        if members.contains_key(name) {
            return Err(CapsuleImportError::invalid(format!(
                "outer member {name:?} appears more than once"
            )));
        }

        let staged = stage_one_member(
            &mut entry,
            staging.path().join(name).as_path(),
            policy,
            &mut staged_total,
        )?;
        members.insert(name, staged);
    }

    if !members.contains_key(INDEX_MEMBER_PATH) {
        return Err(CapsuleImportError::NotV3Bundle);
    }
    for expected in V3_OUTER_MEMBER_PATHS {
        if !members.contains_key(expected) {
            return Err(CapsuleImportError::invalid(format!(
                "outer member {expected:?} is missing; a v3 bundle carries exactly the four \
                 members {}",
                V3_OUTER_MEMBER_PATHS.join(", ")
            )));
        }
    }

    Ok(StagedOuterMembers {
        _staging: staging,
        members,
        staged_total,
    })
}

/// The TAR header's 100-byte name field must be NUL-padded, not NUL-separated.
fn reject_trailing_bytes_in_name_field(
    header: &[u8; 512],
    displayed: &str,
) -> Result<(), CapsuleImportError> {
    const NAME_FIELD: std::ops::Range<usize> = 0..100;
    let field = &header[NAME_FIELD];
    if let Some(terminator) = field.iter().position(|byte| *byte == 0)
        && field[terminator..].iter().any(|byte| *byte != 0)
    {
        return Err(CapsuleImportError::invalid(format!(
            "outer member {displayed:?} has bytes after the NUL terminator in its TAR name \
             field; the member's name is ambiguous"
        )));
    }
    Ok(())
}

/// Copy one member's bytes to `destination` while hashing and counting.
///
/// The declared header size is never consulted for allocation — the buffer is a
/// fixed 64 KiB — so a member that lies about its size is caught later by the
/// digest/size comparison rather than here by an allocation failure.
fn stage_one_member<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    destination: &Path,
    policy: &CapsuleImportPolicy,
    staged_total: &mut u64,
) -> Result<StagedMember, CapsuleImportError> {
    let mut file = File::create(destination)
        .map_err(|source| CapsuleImportError::io("create an outer member staging file", source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size: u64 = 0;

    loop {
        let read = entry
            .read(&mut buffer)
            .map_err(|source| CapsuleImportError::io("read an outer member's bytes", source))?;
        if read == 0 {
            break;
        }
        policy.charge_staged_bytes(staged_total, read as u64)?;
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|source| CapsuleImportError::io("stage an outer member's bytes", source))?;
        size += read as u64;
    }
    file.flush()
        .map_err(|source| CapsuleImportError::io("flush a staged outer member", source))?;

    Ok(StagedMember {
        path: destination.to_path_buf(),
        digest: Sha256Digest::from_raw(hasher.finalize().into()),
        size,
    })
}

/// SHA-256 a file by streaming it in fixed-size chunks.
///
/// Deliberately not `fs::read` + hash: the source archive is the one member with
/// no bound on its size, so reading it whole to hash it would make peak memory a
/// function of untrusted input. The buffer is a fixed 64 KiB regardless.
///
/// # Errors
///
/// [`CapsuleImportError::Io`] on a read failure.
pub(crate) fn hash_file_stream(path: &Path) -> Result<Sha256Digest, CapsuleImportError> {
    let mut file = File::open(path)
        .map_err(|source| CapsuleImportError::io("open a staged member for hashing", source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| CapsuleImportError::io("read a staged member for hashing", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest::from_raw(hasher.finalize().into()))
}

/// SHA-256 the entire byte stream, then rewind it.
///
/// RFC §"Verification": for a Store-fetched bundle this runs **before** any v3
/// parsing, so a bundle that is not the bytes the API named is refused without
/// its contents ever being interpreted.
///
/// # Errors
///
/// [`CapsuleImportError::Io`] on a read or seek failure.
pub(crate) fn hash_whole_stream<R: Read + Seek>(
    reader: &mut R,
) -> Result<Sha256Digest, CapsuleImportError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| CapsuleImportError::io("rewind the bundle stream", source))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| CapsuleImportError::io("read the bundle stream", source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|source| CapsuleImportError::io("rewind the bundle stream", source))?;
    Ok(Sha256Digest::from_raw(hasher.finalize().into()))
}
