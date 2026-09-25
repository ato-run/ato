//! Source pinning and the closure identity a Formation is built from.
//!
//! ## Three identities that are not each other
//!
//! ```text
//! archive transport digest   the BYTES that arrived
//! source tree digest         what those bytes UNPACK TO
//! source closure ref         the tree, plus the rules that measured it
//! ```
//!
//! They fail independently, which is the whole reason they are separate. Intact
//! bytes of a *different* tree pass a byte check and must not pass a tree
//! check: an archive of something else, stored under a colliding key, would
//! otherwise be built and published as this source.
//!
//! ## Provenance
//!
//! The typed proof-state chain is donor code from
//! `deploy/replay-static-lane crates/snapshot-builder/src/source_archive_download.rs`,
//! taken as an algorithm rather than as files. What did NOT come across is the
//! daemon's `AuthoringWork` aggregate, its build-input types and its claim
//! loop: the states are useful, the machine around them is not.
//!
//! The control plane already models the pinned commit and the tree digest
//! (`source_revisions`, `source_materializations`). This crate does not add a
//! parallel model; `SourceClosureRef` is derived from those same facts.

use std::collections::BTreeSet;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Decompress if the bytes are gzipped, otherwise return them unchanged.
///
/// A codeload tarball arrives as `.tar.gz` and a hand-built upload may not.
/// Sniffing the magic rather than trusting a filename means the caller does
/// not have to know which it fetched — and a `tar` reader handed gzip bytes
/// fails with "numeric field did not have utf-8 text", which reads like a
/// corrupt archive rather than a compressed one.
/// Zstandard's magic number, little-endian.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Decompress by what the BYTES say, not by what the URL ended in.
///
/// GitHub serves gzip; an uploaded archive arrives zstd-compressed. Sniffing
/// the magic number rather than trusting a filename or a content-type means a
/// source that arrives by a third transport tomorrow needs nothing here — and
/// that a mislabelled archive is read correctly rather than confidently
/// misread.
fn decompressed(bytes: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, SourceError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|error| {
            SourceError::Unusable(format!("source archive is not readable gzip: {error}"))
        })?;
        return Ok(std::borrow::Cow::Owned(out));
    }
    if bytes.len() >= 4 && bytes[..4] == ZSTD_MAGIC {
        let out = zstd::stream::decode_all(Cursor::new(bytes)).map_err(|error| {
            SourceError::Unusable(format!("source archive is not readable zstd: {error}"))
        })?;
        return Ok(std::borrow::Cow::Owned(out));
    }
    Ok(std::borrow::Cow::Borrowed(bytes))
}

/// How a tree was measured. Part of the closure identity on purpose.
///
/// Two digests produced under different projection rules are not comparable,
/// and a closure ref that omitted the rules would let them look equal.
///
/// v1: a source tree is regular files and directories. Nothing else.
pub const RESOLVER_CONTRACT_V1: &str = "ato.source-resolver.v1";

/// v2: v1, plus contained relative symlinks as entries of their own.
///
/// A symlink is measured as what it IS — a path, the kind `symlink`, and its
/// target string — never flattened into the content it points at: `link -> a`
/// and `link -> b` are different trees even when `a` and `b` hold the same
/// bytes. A link is contained when its target is relative, carries no NUL,
/// and reads as some `..` components followed only by plain names, with no
/// more `..` than the link has parent directories; and when no entry of the
/// tree lies *beneath* a symlink. Together these keep every link, and every
/// chain of links, inside the tree once it is on a disk: a leading `..` walks
/// up real directories only, and plain names never walk up at all. Absolute
/// targets, targets that climb out, targets that climb after descending
/// (`a/../b`), hard links, devices and FIFOs remain refused.
///
/// ## Which version a tree is measured under
///
/// The lowest version whose entry vocabulary covers the tree: a tree with no
/// symlink is measured under v1, exactly as before; a tree with one under v2.
/// The choice is a function of the tree alone — the same files and links give
/// the same version, whether they arrive as a local directory snapshot or an
/// uploaded or codeload archive — so it is canonical, not an implicit switch.
/// It also means no recorded v1 closure changes meaning or needs migrating:
/// every tree v1 could measure still measures to the same v1 digest, and a
/// tree v1 refused now has a v2 identity it never had before.
pub const RESOLVER_CONTRACT_V2: &str = "ato.source-resolver.v2";

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("archive bytes do not match the pinned revision: expected {expected}, got {actual}")]
    ArchiveDigestMismatch { expected: String, actual: String },
    #[error("archive contents do not match the pinned tree: expected {expected}, got {actual}")]
    TreeDigestMismatch { expected: String, actual: String },
    #[error("source archive entry {path} escapes the source root")]
    PathEscape { path: String },
    #[error("source archive contains {kind} at {path}, which a source tree does not define")]
    UnsupportedEntry { kind: &'static str, path: String },
    #[error(
        "source archive contains a symlink at {path} whose target {target:?} is not contained \
         in the source tree ({reason})"
    )]
    SymlinkEscape {
        path: String,
        target: String,
        reason: &'static str,
    },
    #[error("source archive exceeds its {limit} limit")]
    LimitExceeded { limit: &'static str },
    #[error("source subdirectory {path} is not contained")]
    SubdirectoryEscape { path: String },
    #[error("source subdirectory {path} is not present in the tree")]
    SubdirectoryMissing { path: String },
    #[error("{0}")]
    Unusable(String),
}

impl SourceError {
    /// A stable code, so a diagnostic can be matched without parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ArchiveDigestMismatch { .. } => "source_archive_digest_mismatch",
            Self::TreeDigestMismatch { .. } => "source_tree_digest_mismatch",
            Self::PathEscape { .. } => "source_path_escape",
            Self::UnsupportedEntry { .. } => "source_unsupported_entry",
            Self::SymlinkEscape { .. } => "source_symlink_escape",
            Self::LimitExceeded { .. } => "source_limit_exceeded",
            Self::SubdirectoryEscape { .. } => "source_subdirectory_escape",
            Self::SubdirectoryMissing { .. } => "source_subdirectory_missing",
            Self::Unusable(_) => "source_unusable",
        }
    }
}

/// Bounds on what a source tree may be.
///
/// Enforced while reading, not after: a limit checked at the end is a limit
/// that has already let the disk fill.
#[derive(Debug, Clone, Copy)]
pub struct SourceLimits {
    pub max_files: usize,
    pub max_total_bytes: u64,
    pub max_path_depth: usize,
    pub max_path_bytes: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_files: 50_000,
            max_total_bytes: 512 * 1024 * 1024,
            max_path_depth: 64,
            max_path_bytes: 1024,
        }
    }
}

// ─────────────────────────────────────────────────────── the proof-state chain

/// Bytes that arrived. Believed to be nothing yet.
///
/// The bytes are private on purpose: the point of the chain is that unverified
/// content cannot be handed to anything, and a public field would let it.
#[derive(Debug)]
pub struct DownloadedArchive {
    bytes: Vec<u8>,
}

/// The bytes are the ones the pinned revision names.
#[derive(Debug)]
pub struct DigestVerifiedArchive {
    bytes: Vec<u8>,
}

/// The contents are the tree the pinned revision commits to.
///
/// The only state a build accepts. Holding one is proof that both the bytes and
/// what they unpack to were checked.
#[derive(Debug)]
pub struct TreeVerifiedArchive {
    bytes: Vec<u8>,
    tree_digest: String,
    /// The resolver contract the tree was measured under.
    resolver_contract: &'static str,
    /// The content bytes of the tree's regular files, as measured.
    file_bytes: u64,
}

impl DownloadedArchive {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Step one: are these the BYTES the pinned revision names?
    ///
    /// Consumes the value, so the unverified state leaves the caller's hands
    /// rather than merely going unused by it.
    pub fn verify_archive_digest(
        self,
        expected: &str,
    ) -> Result<DigestVerifiedArchive, SourceError> {
        let actual = content_ref(&self.bytes);
        if actual != expected {
            return Err(SourceError::ArchiveDigestMismatch {
                expected: expected.to_owned(),
                actual,
            });
        }
        Ok(DigestVerifiedArchive { bytes: self.bytes })
    }
}

impl DigestVerifiedArchive {
    /// Step two: do those bytes CONTAIN the tree the identity commits to?
    ///
    /// Separate because the two fail independently. Intact bytes of a different
    /// tree pass step one and must not pass step two.
    ///
    /// `expected` is optional: an upload may not know its tree digest yet, and
    /// the tree is still measured and returned either way.
    pub fn verify_tree_digest(
        self,
        expected: Option<&str>,
        limits: SourceLimits,
    ) -> Result<TreeVerifiedArchive, SourceError> {
        let measured = measure(&self.bytes, limits)?;
        if let Some(expected) = expected
            && expected != measured.digest
        {
            return Err(SourceError::TreeDigestMismatch {
                expected: expected.to_owned(),
                actual: measured.digest,
            });
        }
        Ok(TreeVerifiedArchive {
            bytes: self.bytes,
            tree_digest: measured.digest,
            resolver_contract: measured.contract,
            file_bytes: measured.file_bytes,
        })
    }
}

impl TreeVerifiedArchive {
    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }

    /// The archive bytes, reachable only from a verified value.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The resolver contract the tree was measured under (see
    /// [`RESOLVER_CONTRACT_V2`] for how it is chosen).
    pub fn resolver_contract(&self) -> &'static str {
        self.resolver_contract
    }

    /// The logical bytes [`Self::materialize`] writes: the content of the
    /// tree's regular files. Directories and symlinks are zero. Measured
    /// under the same limits, before anything is written.
    pub fn expanded_bytes(&self) -> u64 {
        self.file_bytes
    }

    /// The closure identity: the tree, plus the rules that measured it.
    pub fn closure_ref(&self, subdirectory: &str) -> Result<SourceClosureRef, SourceError> {
        SourceClosureRef::derive(&self.tree_digest, subdirectory, self.resolver_contract)
    }

    /// Expand the tree into `destination`, applying the same containment rules
    /// the measurement used.
    pub fn materialize(
        &self,
        destination: &Path,
        subdirectory: &str,
        limits: SourceLimits,
    ) -> Result<PathBuf, SourceError> {
        let root = expand_archive(&self.bytes, destination, limits)?;
        select_subdirectory(&root, subdirectory)
    }
}

// ─────────────────────────────────────────────────────────── closure identity

/// What a Formation is built from, as one address.
///
/// Deliberately NOT the archive digest. The same tree can arrive as different
/// bytes — a re-tar, a different compression, a different upload — and a
/// closure keyed on bytes would rebuild each time and coalesce nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceClosureRef(String);

impl SourceClosureRef {
    /// Derive from the facts the control plane already stores.
    ///
    /// `source_tree_digest` and `resolver_contract_version` come straight from
    /// `source_revisions`; the subdirectory narrows the tree. Nothing new is
    /// invented, so a closure computed here and a revision recorded there
    /// describe the same thing.
    pub fn derive(
        tree_digest: &str,
        subdirectory: &str,
        resolver_contract_version: &str,
    ) -> Result<Self, SourceError> {
        let subdirectory = normalize_subdirectory(subdirectory)?;
        let mut hasher = Sha256::new();
        // Length-prefixed, so two different field splits cannot hash the same:
        // ("ab", "c") and ("a", "bc") must not collide into one closure.
        for field in [
            resolver_contract_version,
            tree_digest,
            subdirectory.as_str(),
        ] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        Ok(Self(format!("sha256:{:x}", hasher.finalize())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceClosureRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

// ────────────────────────────────────────────────────────────────── measuring

fn content_ref(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// A repository-relative path that cannot leave its repository.
fn normalize_subdirectory(value: &str) -> Result<String, SourceError> {
    let trimmed = value.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if value.starts_with('/') || value.contains('\0') || value.contains('\\') {
        return Err(SourceError::SubdirectoryEscape {
            path: value.to_owned(),
        });
    }
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(SourceError::SubdirectoryEscape {
                path: value.to_owned(),
            });
        }
    }
    Ok(trimmed.to_owned())
}

/// Accept only a plain relative path inside the archive.
///
/// Refused rather than normalized: `..` is not a typo to be corrected, it is a
/// request to write outside the tree.
fn safe_entry_path(path: &Path, limits: SourceLimits) -> Result<PathBuf, SourceError> {
    let display = path.display().to_string();
    if display.len() > limits.max_path_bytes {
        return Err(SourceError::LimitExceeded {
            limit: "max_path_bytes",
        });
    }
    let mut safe = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                safe.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SourceError::PathEscape { path: display });
            }
        }
    }
    if depth > limits.max_path_depth {
        return Err(SourceError::LimitExceeded {
            limit: "max_path_depth",
        });
    }
    if safe.as_os_str().is_empty() {
        return Err(SourceError::PathEscape { path: display });
    }
    Ok(safe)
}

/// PAX and GNU headers describe the ARCHIVE, not the tree inside it.
fn is_archive_metadata(entry_type: tar::EntryType) -> bool {
    matches!(
        entry_type,
        tar::EntryType::XGlobalHeader
            | tar::EntryType::XHeader
            | tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink
    )
}

/// Entry kinds no resolver version defines. (A symlink is defined by v2 and
/// checked by [`contained_symlink_target`].)
fn entry_kind(entry_type: tar::EntryType) -> Option<&'static str> {
    match entry_type {
        tar::EntryType::Link => Some("a hard link"),
        tar::EntryType::Char | tar::EntryType::Block => Some("a device node"),
        tar::EntryType::Fifo => Some("a FIFO"),
        _ => None,
    }
}

/// Measure a source tree: one digest over every path and its content.
///
/// Sorted, so the archive's own entry order cannot change the identity. Both
/// the path and the bytes are length-prefixed for the same reason the closure
/// ref is: a file named `ab` holding `c` must not hash like `a` holding `bc`.
///
/// Symlinks, hard links, devices and FIFOs are refused rather than measured. A
/// source tree is files and directories; the rest are ways to reach outside it
/// once it lands on a disk.
/// Strip a single wrapping directory when EVERY entry shares one.
///
/// A codeload tarball wraps the tree in `<repo>-<sha>/`, which is transport
/// packaging and not part of the source. Measuring it would make the same tree
/// hash differently depending on where it was fetched from — exactly the thing
/// a closure identity exists to prevent. Stripped only when every entry agrees,
/// so an archive that genuinely has a top-level directory keeps it.
fn common_prefix(paths: &[PathBuf]) -> Option<String> {
    let first = paths.first()?.components().next()?;
    let Component::Normal(candidate) = first else {
        return None;
    };
    let candidate = candidate.to_str()?.to_owned();
    // A single-file archive has no wrapper to strip.
    if paths.len() < 2 {
        return None;
    }
    let shares_prefix = |path: &PathBuf| {
        matches!(path.components().next(), Some(Component::Normal(part))
            if part.to_str() == Some(candidate.as_str()))
    };
    // The wrapper's OWN entry is one component deep — a tarball lists the
    // directory as well as its contents — so requiring every path to be deeper
    // than one would reject exactly the archive this is meant to unwrap.
    let all_share = paths.iter().all(shares_prefix);
    let has_contents = paths.iter().any(|path| path.components().count() > 1);
    (all_share && has_contents).then_some(candidate)
}

fn strip_prefix(relative: &Path, prefix: Option<&str>) -> PathBuf {
    match prefix {
        Some(prefix) => relative
            .strip_prefix(prefix)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| relative.to_path_buf()),
        None => relative.to_path_buf(),
    }
}

/// The target of a symlink at `link` (a tree-relative path), if it is
/// contained — the shared rule in [`crate::containment`]. Returned verbatim:
/// the string is the identity, not its resolution.
fn contained_symlink_target(link: &Path, target: &[u8]) -> Result<String, SourceError> {
    crate::containment::validate_contained_symlink_target(link, target).map_err(|escape| {
        SourceError::SymlinkEscape {
            path: link.display().to_string(),
            target: String::from_utf8_lossy(target).into_owned(),
            reason: escape.reason,
        }
    })
}

/// A symlink entry's raw target bytes.
fn link_target_bytes<R: std::io::Read>(entry: &tar::Entry<'_, R>) -> Vec<u8> {
    entry
        .link_name_bytes()
        .map(|bytes| bytes.into_owned())
        .unwrap_or_default()
}

/// Every entry path in an archive, checked for containment. Symlinks are
/// checked for a contained target, and no entry may lie beneath one.
fn entry_paths(archive: &[u8], limits: SourceLimits) -> Result<Vec<PathBuf>, SourceError> {
    let mut paths = Vec::new();
    let mut links: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut tar = tar::Archive::new(Cursor::new(archive));
    for entry in tar
        .entries()
        .map_err(|error| SourceError::Unusable(format!("source archive is unreadable: {error}")))?
    {
        let entry = entry.map_err(|error| {
            SourceError::Unusable(format!("source archive entry is unreadable: {error}"))
        })?;
        // A GitHub tarball opens with `pax_global_header`, which is metadata
        // about the archive rather than a file in the tree. Counting it as a
        // path means no common prefix is ever found, and the wrapper directory
        // survives — which is what left the worker detecting an empty source.
        if is_archive_metadata(entry.header().entry_type()) {
            continue;
        }
        if let Some(kind) = entry_kind(entry.header().entry_type()) {
            return Err(SourceError::UnsupportedEntry {
                kind,
                path: entry
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            });
        }
        let path = entry
            .path()
            .map_err(|error| SourceError::Unusable(format!("entry has no path: {error}")))?
            .into_owned();
        let safe = safe_entry_path(&path, limits)?;
        if entry.header().entry_type() == tar::EntryType::Symlink {
            links.push((safe.clone(), link_target_bytes(&entry)));
        }
        paths.push(safe);
    }
    // Checked against the tree as it will stand, without a transport wrapper:
    // a `..` that only climbs out of `<repo>-<sha>/` has climbed out.
    let prefix = common_prefix(&paths);
    let link_paths: BTreeSet<PathBuf> = links
        .iter()
        .map(|(path, _)| strip_prefix(path, prefix.as_deref()))
        .collect();
    for (path, target) in &links {
        contained_symlink_target(&strip_prefix(path, prefix.as_deref()), target)?;
    }
    for path in &paths {
        let relative = strip_prefix(path, prefix.as_deref());
        if relative
            .ancestors()
            .skip(1)
            .any(|ancestor| link_paths.contains(ancestor))
        {
            return Err(SourceError::SymlinkEscape {
                path: relative.display().to_string(),
                target: String::new(),
                reason: "an entry lies beneath a symlink",
            });
        }
    }
    Ok(paths)
}

pub fn measure_source_tree(archive: &[u8], limits: SourceLimits) -> Result<String, SourceError> {
    measure_source_tree_contract(archive, limits).map(|(digest, _)| digest)
}

/// [`measure_source_tree`], with the resolver contract the tree was measured
/// under: v1 for a tree of files and directories, v2 when it holds a symlink.
pub fn measure_source_tree_contract(
    archive: &[u8],
    limits: SourceLimits,
) -> Result<(String, &'static str), SourceError> {
    measure(archive, limits).map(|measured| (measured.digest, measured.contract))
}

/// A tree as measured: its digest, the contract it was measured under, and
/// the content bytes of its regular files.
struct Measured {
    digest: String,
    contract: &'static str,
    file_bytes: u64,
}

fn measure(archive: &[u8], limits: SourceLimits) -> Result<Measured, SourceError> {
    let mut entries: BTreeSet<(String, String)> = BTreeSet::new();
    let mut has_symlink = false;
    let mut total: u64 = 0;
    let mut count = 0usize;

    let archive = decompressed(archive)?;
    let prefix = common_prefix(&entry_paths(archive.as_ref(), limits)?);
    let mut tar = tar::Archive::new(Cursor::new(archive.as_ref()));
    for entry in tar
        .entries()
        .map_err(|error| SourceError::Unusable(format!("source archive is unreadable: {error}")))?
    {
        let mut entry = entry.map_err(|error| {
            SourceError::Unusable(format!("source archive entry is unreadable: {error}"))
        })?;
        let entry_type = entry.header().entry_type();
        if is_archive_metadata(entry_type) {
            continue;
        }
        if let Some(kind) = entry_kind(entry_type) {
            return Err(SourceError::UnsupportedEntry {
                kind,
                path: entry
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            });
        }
        let path = entry
            .path()
            .map_err(|error| SourceError::Unusable(format!("entry has no path: {error}")))?
            .into_owned();
        let relative = strip_prefix(&safe_entry_path(&path, limits)?, prefix.as_deref());
        if relative.as_os_str().is_empty() {
            continue;
        }

        if entry_type.is_dir() {
            // Directories are part of the tree: an empty `fixtures/` that
            // vanished would be a different source.
            entries.insert((format!("{}/", relative.display()), String::new()));
            continue;
        }
        if entry_type == tar::EntryType::Symlink {
            // The link itself: kind and target, never the content behind it.
            // `symlink:` cannot collide with a file's `sha256:` digest.
            let target = contained_symlink_target(&relative, &link_target_bytes(&entry))?;
            has_symlink = true;
            count += 1;
            if count > limits.max_files {
                return Err(SourceError::LimitExceeded { limit: "max_files" });
            }
            entries.insert((relative.display().to_string(), format!("symlink:{target}")));
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }

        count += 1;
        if count > limits.max_files {
            return Err(SourceError::LimitExceeded { limit: "max_files" });
        }
        let size = entry.header().size().unwrap_or(0);
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(SourceError::LimitExceeded {
                limit: "max_total_bytes",
            });
        }

        let mut bytes = Vec::with_capacity(size as usize);
        std::io::Read::read_to_end(&mut entry, &mut bytes)
            .map_err(|error| SourceError::Unusable(format!("entry is unreadable: {error}")))?;
        entries.insert((relative.display().to_string(), content_ref(&bytes)));
    }

    // The lowest contract that defines every entry (see RESOLVER_CONTRACT_V2).
    let contract = if has_symlink {
        RESOLVER_CONTRACT_V2
    } else {
        RESOLVER_CONTRACT_V1
    };
    let mut hasher = Sha256::new();
    hasher.update(contract.as_bytes());
    hasher.update([0]);
    for (path, digest) in &entries {
        for field in [path.as_str(), digest.as_str()] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
    }
    Ok(Measured {
        digest: format!("sha256:{:x}", hasher.finalize()),
        contract,
        file_bytes: total,
    })
}

/// Expand into a staging directory, applying the same rules the measurement did.
fn expand_archive(
    archive: &[u8],
    destination: &Path,
    limits: SourceLimits,
) -> Result<PathBuf, SourceError> {
    std::fs::create_dir_all(destination).map_err(|error| {
        SourceError::Unusable(format!("cannot create {}: {error}", destination.display()))
    })?;

    let archive = decompressed(archive)?;
    let prefix = common_prefix(&entry_paths(archive.as_ref(), limits)?);
    let mut tar = tar::Archive::new(Cursor::new(archive.as_ref()));
    tar.set_preserve_permissions(false);
    tar.set_unpack_xattrs(false);
    tar.set_overwrite(true);

    let mut total: u64 = 0;
    for entry in tar
        .entries()
        .map_err(|error| SourceError::Unusable(format!("source archive is unreadable: {error}")))?
    {
        let mut entry = entry.map_err(|error| {
            SourceError::Unusable(format!("source archive entry is unreadable: {error}"))
        })?;
        let entry_type = entry.header().entry_type();
        if is_archive_metadata(entry_type) {
            continue;
        }
        if let Some(kind) = entry_kind(entry_type) {
            return Err(SourceError::UnsupportedEntry {
                kind,
                path: entry
                    .path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            });
        }
        let path = entry
            .path()
            .map_err(|error| SourceError::Unusable(format!("entry has no path: {error}")))?
            .into_owned();
        let relative = strip_prefix(&safe_entry_path(&path, limits)?, prefix.as_deref());
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(&relative);

        if entry_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| {
                SourceError::Unusable(format!("cannot create {}: {error}", relative.display()))
            })?;
            continue;
        }
        if entry_type == tar::EntryType::Symlink {
            // Re-checked here as well: expansion must never depend on the
            // measurement having been run first.
            let link = contained_symlink_target(&relative, &link_target_bytes(&entry))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    SourceError::Unusable(format!("cannot create {}: {error}", parent.display()))
                })?;
            }
            create_symlink(&link, &target).map_err(|error| {
                SourceError::Unusable(format!("cannot link {}: {error}", relative.display()))
            })?;
            continue;
        }
        if !entry_type.is_file() {
            continue;
        }
        total = total.saturating_add(entry.header().size().unwrap_or(0));
        if total > limits.max_total_bytes {
            return Err(SourceError::LimitExceeded {
                limit: "max_total_bytes",
            });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                SourceError::Unusable(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        entry.unpack(&target).map_err(|error| {
            SourceError::Unusable(format!("cannot write {}: {error}", relative.display()))
        })?;
    }
    Ok(destination.to_path_buf())
}

/// Recreate a link as a link; its target is never read or followed here.
#[cfg(unix)]
fn create_symlink(link: &str, at: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(at) {
        Ok(existing) if existing.is_dir() && !existing.is_symlink() => std::fs::remove_dir_all(at)?,
        Ok(_) => std::fs::remove_file(at)?,
        Err(_) => {}
    }
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn create_symlink(_link: &str, _at: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "symlinks in a source tree are materialized on Unix only",
    ))
}

/// Narrow an expanded tree to its declared subdirectory.
///
/// Re-checked against the REAL path rather than the string: the string check
/// happened before anything existed on a disk, and cannot see a directory that
/// resolves elsewhere.
fn select_subdirectory(root: &Path, subdirectory: &str) -> Result<PathBuf, SourceError> {
    let normalized = normalize_subdirectory(subdirectory)?;
    if normalized.is_empty() {
        return Ok(root.to_path_buf());
    }
    let candidate = root.join(&normalized);
    if !candidate.is_dir() {
        return Err(SourceError::SubdirectoryMissing { path: normalized });
    }
    let canonical_root = root.canonicalize().map_err(|error| {
        SourceError::Unusable(format!("cannot resolve the source root: {error}"))
    })?;
    let canonical = candidate.canonicalize().map_err(|error| {
        SourceError::Unusable(format!("cannot resolve the subdirectory: {error}"))
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(SourceError::SubdirectoryEscape { path: normalized });
    }
    Ok(canonical)
}

// ────────────────────────────────────────────────────────────────── redaction

/// A URL with its credentials and query removed.
///
/// Upload and download grants are pre-signed: the query string IS the
/// credential. Naming which URL failed is useful; printing it is handing the
/// grant to whoever reads the log.
pub fn redact_url(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return "<redacted>".to_owned();
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let host_and_path = match rest.split_once('@') {
        // Anything before an `@` is userinfo.
        Some((_, after)) => after,
        None => rest,
    };
    format!("{scheme}://{host_and_path}")
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Build a tar with the given entries. `mtime` varies so the tests can show
    /// that transport metadata does not reach the tree identity.
    fn archive(entries: &[(&str, &[u8])], mtime: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for (name, contents) in entries {
                let mut header = tar::Header::new_ustar();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(mtime);
                header.set_entry_type(tar::EntryType::Regular);
                builder
                    .append_data(&mut header, name, Cursor::new(*contents))
                    .expect("append");
            }
            builder.finish().expect("finish");
        }
        bytes
    }

    fn forge(name: &str, entry_type: tar::EntryType) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let contents = b"x";
            let mut header = tar::Header::new_ustar();
            header.set_size(if entry_type.is_file() {
                contents.len() as u64
            } else {
                0
            });
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_entry_type(entry_type);
            // The `tar` crate refuses to WRITE a `..` path, so a hostile archive
            // is forged by hand — which is exactly the archive an attacker
            // sends, and the reason the reader must check.
            let raw = header.as_old_mut();
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
            if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
                let target = b"/etc/passwd";
                raw.linkname[..target.len()].copy_from_slice(target);
            }
            header.set_cksum();
            builder
                .append(&header, Cursor::new(&contents[..]))
                .expect("append");
            builder.finish().expect("finish");
        }
        bytes
    }

    const LIMITS: SourceLimits = SourceLimits {
        max_files: 50_000,
        max_total_bytes: 512 * 1024 * 1024,
        max_path_depth: 64,
        max_path_bytes: 1024,
    };

    #[test]
    fn the_same_tree_from_different_bytes_has_one_closure() {
        // Scenario C. A re-tar, a different compression, a different upload —
        // the bytes differ and the source does not. A closure keyed on bytes
        // would rebuild every time and coalesce nothing.
        let first = archive(&[("app.py", b"print(1)\n"), ("README", b"hi\n")], 0);
        let second = archive(
            &[("README", b"hi\n"), ("app.py", b"print(1)\n")],
            1_700_000_000,
        );
        assert_ne!(
            content_ref(&first),
            content_ref(&second),
            "bytes must differ"
        );

        let tree_one = measure_source_tree(&first, LIMITS).expect("measures");
        let tree_two = measure_source_tree(&second, LIMITS).expect("measures");
        assert_eq!(
            tree_one, tree_two,
            "entry order and mtime are not the source"
        );

        let closure_one = SourceClosureRef::derive(&tree_one, "", RESOLVER_CONTRACT_V1).unwrap();
        let closure_two = SourceClosureRef::derive(&tree_two, "", RESOLVER_CONTRACT_V1).unwrap();
        assert_eq!(closure_one, closure_two);
    }

    #[test]
    fn a_closure_is_not_its_archive_digest() {
        let bytes = archive(&[("app.py", b"print(1)\n")], 0);
        let tree = measure_source_tree(&bytes, LIMITS).expect("measures");
        let closure = SourceClosureRef::derive(&tree, "", RESOLVER_CONTRACT_V1).unwrap();
        // Three identities, none of which may stand in for another.
        assert_ne!(closure.as_str(), content_ref(&bytes));
        assert_ne!(closure.as_str(), tree.as_str());
    }

    #[test]
    fn different_content_is_a_different_tree() {
        let one = measure_source_tree(&archive(&[("a", b"x")], 0), LIMITS).unwrap();
        let two = measure_source_tree(&archive(&[("a", b"y")], 0), LIMITS).unwrap();
        let moved = measure_source_tree(&archive(&[("b", b"x")], 0), LIMITS).unwrap();
        assert_ne!(one, two, "content must matter");
        assert_ne!(one, moved, "path must matter");
    }

    #[test]
    fn field_boundaries_cannot_be_confused() {
        // Without length prefixes, ("ab","c") and ("a","bc") would hash alike —
        // two different sources sharing one closure.
        let one = SourceClosureRef::derive("sha256:ab", "c", RESOLVER_CONTRACT_V1).unwrap();
        let two = SourceClosureRef::derive("sha256:a", "bc", RESOLVER_CONTRACT_V1).unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn the_resolver_contract_is_part_of_the_identity() {
        // Two digests produced under different projection rules are not
        // comparable, and a ref that omitted the rules would let them look equal.
        let one = SourceClosureRef::derive("sha256:aa", "", RESOLVER_CONTRACT_V1).unwrap();
        let two = SourceClosureRef::derive("sha256:aa", "", "ato.source-resolver.v2").unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn intact_bytes_of_the_wrong_tree_do_not_pass() {
        // The two checks fail independently: an archive of something else,
        // stored under a colliding key, must not reach a build.
        let bytes = archive(&[("app.py", b"print(1)\n")], 0);
        let digest = content_ref(&bytes);
        let verified = DownloadedArchive::new(bytes)
            .verify_archive_digest(&digest)
            .expect("bytes are right");
        let error = verified
            .verify_tree_digest(Some(&format!("sha256:{}", "0".repeat(64))), LIMITS)
            .unwrap_err();
        assert_eq!(error.code(), "source_tree_digest_mismatch");
    }

    #[test]
    fn wrong_bytes_are_refused_before_the_tree_is_read() {
        let bytes = archive(&[("app.py", b"print(1)\n")], 0);
        let error = DownloadedArchive::new(bytes)
            .verify_archive_digest(&format!("sha256:{}", "0".repeat(64)))
            .unwrap_err();
        assert_eq!(error.code(), "source_archive_digest_mismatch");
    }

    #[test]
    fn a_traversing_entry_is_refused() {
        for name in ["../escape", "nested/../../escape", "/etc/passwd"] {
            let bytes = forge(name, tar::EntryType::Regular);
            let error = measure_source_tree(&bytes, LIMITS).unwrap_err();
            assert_eq!(error.code(), "source_path_escape", "{name}");
        }
    }

    #[test]
    fn hard_links_devices_and_fifos_are_refused() {
        // A source tree is files, directories and contained symlinks. The
        // rest are ways to reach outside it once it lands on a disk.
        for entry_type in [
            tar::EntryType::Link,
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Fifo,
        ] {
            let bytes = forge("payload", entry_type);
            let error = measure_source_tree(&bytes, LIMITS).unwrap_err();
            assert_eq!(error.code(), "source_unsupported_entry", "{entry_type:?}");
        }
    }

    #[test]
    fn limits_are_enforced_while_reading() {
        let tiny = SourceLimits {
            max_files: 1,
            max_total_bytes: 8,
            max_path_depth: 2,
            max_path_bytes: 16,
        };
        let too_many = archive(&[("a", b"x"), ("b", b"y")], 0);
        assert_eq!(
            measure_source_tree(&too_many, tiny).unwrap_err().code(),
            "source_limit_exceeded"
        );
        let too_big = archive(&[("a", b"0123456789")], 0);
        assert_eq!(
            measure_source_tree(&too_big, tiny).unwrap_err().code(),
            "source_limit_exceeded"
        );
        let too_deep = archive(&[("a/b/c/d", b"x")], 0);
        assert_eq!(
            measure_source_tree(&too_deep, tiny).unwrap_err().code(),
            "source_limit_exceeded"
        );
    }

    #[test]
    fn the_expanded_bytes_are_known_and_bounded_before_expansion() {
        let bytes = archive(&[("a", b"12345"), ("dir/b", b"678")], 0);
        let digest = content_ref(&bytes);
        let under = SourceLimits {
            max_total_bytes: 7,
            ..LIMITS
        };
        // Over the limit: refused while measuring, before a file exists.
        let refused = DownloadedArchive::new(bytes.clone())
            .verify_archive_digest(&digest)
            .expect("bytes")
            .verify_tree_digest(None, under)
            .unwrap_err();
        assert!(matches!(
            refused,
            SourceError::LimitExceeded {
                limit: "max_total_bytes"
            }
        ));

        // Within it: the measured bytes are exactly what expansion writes.
        let verified = DownloadedArchive::new(bytes)
            .verify_archive_digest(&digest)
            .expect("bytes")
            .verify_tree_digest(None, LIMITS)
            .expect("tree");
        assert_eq!(verified.expanded_bytes(), 8);
        let scratch = tempfile::tempdir().expect("scratch");
        let root = verified
            .materialize(&scratch.path().join("tree"), "", LIMITS)
            .expect("expands");
        let written = std::fs::metadata(root.join("a")).unwrap().len()
            + std::fs::metadata(root.join("dir/b")).unwrap().len();
        assert_eq!(written, verified.expanded_bytes());

        // Expansion holds the limit by itself too, part-way through.
        let refused = verified
            .materialize(&scratch.path().join("again"), "", under)
            .unwrap_err();
        assert!(matches!(
            refused,
            SourceError::LimitExceeded {
                limit: "max_total_bytes"
            }
        ));
    }

    #[test]
    fn a_subdirectory_cannot_escape_its_repository() {
        for candidate in ["../elsewhere", "/etc", "app/../../secrets", "..", "a\\b"] {
            let error =
                SourceClosureRef::derive("sha256:aa", candidate, RESOLVER_CONTRACT_V1).unwrap_err();
            assert_eq!(error.code(), "source_subdirectory_escape", "{candidate}");
        }
        SourceClosureRef::derive("sha256:aa", "services/api", RESOLVER_CONTRACT_V1)
            .expect("a plain relative path is fine");
    }

    #[test]
    fn a_subdirectory_narrows_the_closure() {
        // Two Formations of the same repository at different subdirectories are
        // different builds, and must not coalesce.
        let root = SourceClosureRef::derive("sha256:aa", "", RESOLVER_CONTRACT_V1).unwrap();
        let nested =
            SourceClosureRef::derive("sha256:aa", "services/api", RESOLVER_CONTRACT_V1).unwrap();
        assert_ne!(root, nested);
        // Leading and trailing slashes are the same request.
        assert_eq!(
            nested,
            SourceClosureRef::derive("sha256:aa", "services/api/", RESOLVER_CONTRACT_V1).unwrap()
        );
    }

    #[test]
    fn a_verified_archive_materializes_and_narrows() {
        let bytes = archive(
            &[
                ("README", b"root\n"),
                ("services/api/app.py", b"print(1)\n"),
            ],
            0,
        );
        let digest = content_ref(&bytes);
        let verified = DownloadedArchive::new(bytes)
            .verify_archive_digest(&digest)
            .expect("bytes")
            .verify_tree_digest(None, LIMITS)
            .expect("tree");

        let staging = tempfile::tempdir().expect("tempdir");
        let root = verified
            .materialize(staging.path(), "", LIMITS)
            .expect("materializes");
        assert!(root.join("README").is_file());

        let narrowed = verified
            .materialize(&staging.path().join("second"), "services/api", LIMITS)
            .expect("narrows");
        assert!(narrowed.join("app.py").is_file());
        assert!(!narrowed.join("README").exists());
    }

    #[test]
    fn a_missing_subdirectory_is_refused_rather_than_created() {
        let bytes = archive(&[("README", b"root\n")], 0);
        let digest = content_ref(&bytes);
        let verified = DownloadedArchive::new(bytes)
            .verify_archive_digest(&digest)
            .unwrap()
            .verify_tree_digest(None, LIMITS)
            .unwrap();
        let staging = tempfile::tempdir().expect("tempdir");
        let error = verified
            .materialize(staging.path(), "services/api", LIMITS)
            .unwrap_err();
        // Creating it would build an empty directory and call it the source.
        assert_eq!(error.code(), "source_subdirectory_missing");
    }

    #[test]
    fn a_grant_url_never_reaches_a_diagnostic_intact() {
        // A pre-signed URL's query string IS the credential. Naming which URL
        // failed is useful; printing it hands the grant to whoever reads the log.
        for (raw, expected) in [
            (
                "https://store.example.com/o/abc?X-Amz-Signature=deadbeef&X-Amz-Expires=900",
                "https://store.example.com/o/abc",
            ),
            (
                "https://user:hunter2@store.example.com/o/abc",
                "https://store.example.com/o/abc",
            ),
            (
                "https://store.example.com/o/abc#frag",
                "https://store.example.com/o/abc",
            ),
        ] {
            let redacted = redact_url(raw);
            assert_eq!(redacted, expected);
            assert!(!redacted.contains("deadbeef"));
            assert!(!redacted.contains("hunter2"));
        }
        assert_eq!(redact_url("not a url"), "<redacted>");
    }

    #[test]
    fn an_empty_directory_is_part_of_the_tree() {
        // A `fixtures/` that vanished would be a different source.
        let mut with_dir = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut with_dir);
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Directory);
            builder
                .append_data(&mut header, "fixtures/", std::io::empty())
                .expect("append");
            builder.finish().expect("finish");
        }
        let empty = archive(&[], 0);
        assert_ne!(
            measure_source_tree(&with_dir, LIMITS).unwrap(),
            measure_source_tree(&empty, LIMITS).unwrap()
        );
    }

    fn gzipped(raw: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(raw).expect("gzip");
        encoder.finish().expect("gzip")
    }

    #[test]
    fn a_gzipped_archive_measures_the_same_as_a_plain_one() {
        // A codeload tarball arrives gzipped and a hand-built upload may not.
        // A tar reader handed gzip bytes fails with "numeric field did not have
        // utf-8 text", which reads like a corrupt archive rather than a
        // compressed one — observed on the acceptance host.
        let plain = archive(&[("app.py", b"print(1)\n")], 0);
        let compressed = gzipped(&plain);
        assert_eq!(
            measure_source_tree(&plain, LIMITS).unwrap(),
            measure_source_tree(&compressed, LIMITS).unwrap()
        );
    }

    /// Like `archive`, but also lists the wrapper directory itself — which a
    /// real tarball does, and which is what made the first fix miss.
    fn wrapped_archive(prefix: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Directory);
            builder
                .append_data(&mut header, format!("{prefix}/"), std::io::empty())
                .expect("append");
            for (name, contents) in entries {
                let mut header = tar::Header::new_ustar();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                header.set_entry_type(tar::EntryType::Regular);
                builder
                    .append_data(
                        &mut header,
                        format!("{prefix}/{name}"),
                        Cursor::new(*contents),
                    )
                    .expect("append");
            }
            builder.finish().expect("finish");
        }
        bytes
    }

    #[test]
    fn a_wrapper_listed_as_its_own_entry_is_still_stripped() {
        // A real tarball lists `<repo>-<sha>/` as a directory entry alongside
        // its contents. Requiring every path to be deeper than one component
        // rejected exactly the archive this exists to unwrap — which is how the
        // worker ended up detecting an empty source.
        let bare = archive(&[("app.py", b"print(1)\n"), ("README", b"hi\n")], 0);
        let wrapped = wrapped_archive(
            "fixture-922b112",
            &[("app.py", b"print(1)\n"), ("README", b"hi\n")],
        );
        assert_eq!(
            measure_source_tree(&bare, LIMITS).unwrap(),
            measure_source_tree(&wrapped, LIMITS).unwrap()
        );

        let digest = content_ref(&wrapped);
        let verified = DownloadedArchive::new(wrapped)
            .verify_archive_digest(&digest)
            .unwrap()
            .verify_tree_digest(None, LIMITS)
            .unwrap();
        let staging = tempfile::tempdir().expect("tempdir");
        let root = verified.materialize(staging.path(), "", LIMITS).unwrap();
        assert!(root.join("app.py").is_file());
        assert!(!root.join("fixture-922b112").exists());
    }

    #[test]
    fn a_transport_wrapper_directory_is_not_part_of_the_source() {
        // codeload wraps the tree in `<repo>-<sha>/`. Measuring that would make
        // the same tree hash differently depending on where it was fetched
        // from, which is exactly what a closure identity exists to prevent.
        let bare = archive(&[("app.py", b"print(1)\n"), ("README", b"hi\n")], 0);
        let wrapped = archive(
            &[
                ("repo-922b112/app.py", b"print(1)\n"),
                ("repo-922b112/README", b"hi\n"),
            ],
            0,
        );
        assert_eq!(
            measure_source_tree(&bare, LIMITS).unwrap(),
            measure_source_tree(&wrapped, LIMITS).unwrap()
        );
    }

    #[test]
    fn a_real_top_level_directory_is_kept() {
        // Stripping happens only when EVERY entry shares one prefix. A source
        // that genuinely has `src/` alongside `README` keeps it.
        let with_dir = archive(&[("src/app.py", b"1"), ("README", b"hi")], 0);
        let flattened = archive(&[("app.py", b"1"), ("README", b"hi")], 0);
        assert_ne!(
            measure_source_tree(&with_dir, LIMITS).unwrap(),
            measure_source_tree(&flattened, LIMITS).unwrap()
        );
    }

    #[test]
    fn a_wrapped_archive_materializes_without_its_wrapper() {
        let wrapped = archive(
            &[
                ("repo-922b112/app.py", b"print(1)\n"),
                ("repo-922b112/lib/util.py", b"x = 1\n"),
            ],
            0,
        );
        let digest = content_ref(&wrapped);
        let verified = DownloadedArchive::new(wrapped)
            .verify_archive_digest(&digest)
            .expect("bytes")
            .verify_tree_digest(None, LIMITS)
            .expect("tree");
        let staging = tempfile::tempdir().expect("tempdir");
        let root = verified
            .materialize(staging.path(), "", LIMITS)
            .expect("materializes");
        // `/app` must contain app.py, not repo-922b112/app.py.
        assert!(root.join("app.py").is_file());
        assert!(root.join("lib/util.py").is_file());
        assert!(!root.join("repo-922b112").exists());
    }

    // ── resolver v2: contained symlinks ─────────────────────────────────────

    enum E<'a> {
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
        Dir(&'a str),
    }

    fn tree(entries: &[E<'_>]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for entry in entries {
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_mtime(0);
                match entry {
                    E::File(path, contents) => {
                        header.set_size(contents.len() as u64);
                        header.set_entry_type(tar::EntryType::Regular);
                        builder
                            .append_data(&mut header, path, Cursor::new(*contents))
                            .expect("file");
                    }
                    E::Link(path, target) => {
                        header.set_size(0);
                        header.set_entry_type(tar::EntryType::Symlink);
                        builder
                            .append_link(&mut header, path, target)
                            .expect("link");
                    }
                    E::Dir(path) => {
                        header.set_size(0);
                        header.set_entry_type(tar::EntryType::Directory);
                        builder
                            .append_data(&mut header, path, std::io::empty())
                            .expect("dir");
                    }
                }
            }
            builder.finish().expect("finish");
        }
        bytes
    }

    fn contract(bytes: &[u8]) -> Result<(String, &'static str), SourceError> {
        measure_source_tree_contract(bytes, LIMITS)
    }

    #[test]
    fn a_tree_without_symlinks_keeps_its_v1_digest() {
        // Recomputed here from the v1 definition, independently of the
        // measuring code: every tree v1 could measure keeps its identity.
        // (A top-level file, so `safe/` is not taken for a transport wrapper.)
        let bytes = tree(&[
            E::File("README", b"r"),
            E::Dir("safe/"),
            E::File("safe/file.txt", b"hello"),
        ]);
        let (digest, version) = contract(&bytes).expect("measures");
        assert_eq!(version, RESOLVER_CONTRACT_V1);
        let mut hasher = Sha256::new();
        hasher.update(RESOLVER_CONTRACT_V1.as_bytes());
        hasher.update([0]);
        let readme = format!("sha256:{:x}", Sha256::digest(b"r"));
        let file = format!("sha256:{:x}", Sha256::digest(b"hello"));
        for (path, value) in [
            ("README", readme.as_str()),
            ("safe/", ""),
            ("safe/file.txt", file.as_str()),
        ] {
            for field in [path, value] {
                hasher.update((field.len() as u64).to_be_bytes());
                hasher.update(field.as_bytes());
            }
        }
        assert_eq!(digest, format!("sha256:{:x}", hasher.finalize()));
    }

    #[test]
    fn a_contained_symlink_is_measured_under_v2_as_a_link() {
        let plain = tree(&[E::Dir("safe/"), E::File("safe/file.txt", b"hello")]);
        let linked = tree(&[
            E::Dir("safe/"),
            E::File("safe/file.txt", b"hello"),
            E::Link("link", "safe/file.txt"),
        ]);
        let (plain_digest, plain_version) = contract(&plain).unwrap();
        let (linked_digest, linked_version) = contract(&linked).unwrap();
        assert_eq!(plain_version, RESOLVER_CONTRACT_V1);
        assert_eq!(linked_version, RESOLVER_CONTRACT_V2);
        assert_ne!(plain_digest, linked_digest);
        let verified = DigestVerifiedArchive { bytes: linked }
            .verify_tree_digest(None, LIMITS)
            .unwrap();
        assert_eq!(verified.resolver_contract(), RESOLVER_CONTRACT_V2);
        assert_eq!(
            verified.closure_ref("").unwrap(),
            SourceClosureRef::derive(&linked_digest, "", RESOLVER_CONTRACT_V2).unwrap()
        );
    }

    #[test]
    fn the_link_target_is_the_identity_not_the_content_behind_it() {
        let to_a = tree(&[
            E::File("a", b"same"),
            E::File("b", b"same"),
            E::Link("link", "a"),
        ]);
        let to_b = tree(&[
            E::File("a", b"same"),
            E::File("b", b"same"),
            E::Link("link", "b"),
        ]);
        assert_ne!(contract(&to_a).unwrap().0, contract(&to_b).unwrap().0);
        // And a link is not a copy of its target.
        let copy = tree(&[
            E::File("a", b"same"),
            E::File("b", b"same"),
            E::File("link", b"same"),
        ]);
        assert_ne!(contract(&to_a).unwrap().0, contract(&copy).unwrap().0);
    }

    #[test]
    fn contained_targets_are_accepted() {
        for (link, target) in [
            ("assets/current", "versions/v1"),
            ("foo/link", "../shared"),
            ("a/b/link", "../../c"),
            ("sub/link", ".."),
            ("link", "./safe/file.txt"),
        ] {
            let bytes = tree(&[E::Link(link, target)]);
            assert!(contract(&bytes).is_ok(), "{link} -> {target}");
        }
    }

    #[test]
    fn escaping_targets_are_refused() {
        for (link, target) in [
            ("link", "/etc/passwd"),
            ("link", ".."),
            ("link", "../outside"),
            ("sub/link", "../../outside"),
            ("foo/link", "../../../outside"),
            // Descending first and climbing afterwards passes through a name
            // that may itself be a link; refused even when it would land inside.
            ("link", "foo/../../outside"),
            ("link", "a/../b"),
        ] {
            let bytes = tree(&[E::Link(link, target)]);
            let error = contract(&bytes).expect_err(&format!("{link} -> {target}"));
            assert_eq!(error.code(), "source_symlink_escape", "{link} -> {target}");
            // Expansion refuses on its own, too.
            let destination = tempfile::tempdir().unwrap();
            let error = expand_archive(&bytes, destination.path(), LIMITS).unwrap_err();
            assert_eq!(error.code(), "source_symlink_escape");
        }
    }

    #[test]
    fn nothing_may_lie_beneath_a_symlink() {
        // `dir -> real`, then `dir/file`: extracting it would write through
        // the link.
        let bytes = tree(&[
            E::Dir("real/"),
            E::Link("dir", "real"),
            E::File("dir/file", b"x"),
        ]);
        assert_eq!(
            contract(&bytes).unwrap_err().code(),
            "source_symlink_escape"
        );
    }

    #[test]
    fn a_chain_of_links_cannot_climb_out() {
        // `s -> .` is contained, and `a -> s/s/../..` would read as contained
        // lexically but climb out through `s` on a disk: the climb after a
        // descent is refused.
        let bytes = tree(&[E::Link("s", "."), E::Link("a", "s/s/../..")]);
        assert_eq!(
            contract(&bytes).unwrap_err().code(),
            "source_symlink_escape"
        );
    }

    #[test]
    fn a_wrapped_archive_with_links_measures_like_the_bare_tree() {
        let bare = tree(&[
            E::Dir("safe/"),
            E::File("safe/file.txt", b"hello"),
            E::Link("link", "safe/file.txt"),
        ]);
        let wrapped = tree(&[
            E::Dir("repo-abc123/"),
            E::Dir("repo-abc123/safe/"),
            E::File("repo-abc123/safe/file.txt", b"hello"),
            E::Link("repo-abc123/link", "safe/file.txt"),
        ]);
        assert_eq!(contract(&bare).unwrap(), contract(&wrapped).unwrap());
        // A climb that only leaves the wrapper has left the tree.
        let climbing = tree(&[
            E::Dir("repo-abc123/"),
            E::File("repo-abc123/file", b"x"),
            E::Link("repo-abc123/link", "../repo-abc123/file"),
        ]);
        assert_eq!(
            contract(&climbing).unwrap_err().code(),
            "source_symlink_escape"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_materialized_as_a_link() {
        let bytes = tree(&[
            E::Dir("safe/"),
            E::File("safe/file.txt", b"hello"),
            E::Link("link", "safe/file.txt"),
        ]);
        let verified = DigestVerifiedArchive { bytes }
            .verify_tree_digest(None, LIMITS)
            .unwrap();
        let destination = tempfile::tempdir().unwrap();
        let root = verified
            .materialize(&destination.path().join("tree"), "", LIMITS)
            .unwrap();
        let link = root.join("link");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("safe/file.txt")
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"hello");
    }

    #[test]
    fn links_count_against_the_file_limit() {
        let tiny = SourceLimits {
            max_files: 1,
            ..LIMITS
        };
        let bytes = tree(&[E::File("a", b"x"), E::Link("b", "a")]);
        let error = measure_source_tree(&bytes, tiny).unwrap_err();
        assert_eq!(error.code(), "source_limit_exceeded");
    }
}
