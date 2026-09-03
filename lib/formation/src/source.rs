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
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// How a tree was measured. Part of the closure identity on purpose.
///
/// Two digests produced under different projection rules are not comparable,
/// and a closure ref that omitted the rules would let them look equal.
pub const RESOLVER_CONTRACT_V1: &str = "ato.source-resolver.v1";

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
        let tree_digest = measure_source_tree(&self.bytes, limits)?;
        if let Some(expected) = expected
            && expected != tree_digest
        {
            return Err(SourceError::TreeDigestMismatch {
                expected: expected.to_owned(),
                actual: tree_digest,
            });
        }
        Ok(TreeVerifiedArchive {
            bytes: self.bytes,
            tree_digest,
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

    /// The closure identity: the tree, plus the rules that measured it.
    pub fn closure_ref(&self, subdirectory: &str) -> Result<SourceClosureRef, SourceError> {
        SourceClosureRef::derive(&self.tree_digest, subdirectory, RESOLVER_CONTRACT_V1)
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

fn entry_kind(entry_type: tar::EntryType) -> Option<&'static str> {
    match entry_type {
        tar::EntryType::Symlink => Some("a symlink"),
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
pub fn measure_source_tree(archive: &[u8], limits: SourceLimits) -> Result<String, SourceError> {
    let mut entries: BTreeSet<(String, String)> = BTreeSet::new();
    let mut total: u64 = 0;
    let mut count = 0usize;

    let mut tar = tar::Archive::new(Cursor::new(archive));
    for entry in tar
        .entries()
        .map_err(|error| SourceError::Unusable(format!("source archive is unreadable: {error}")))?
    {
        let mut entry = entry.map_err(|error| {
            SourceError::Unusable(format!("source archive entry is unreadable: {error}"))
        })?;
        let entry_type = entry.header().entry_type();
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
        let relative = safe_entry_path(&path, limits)?;

        if entry_type.is_dir() {
            // Directories are part of the tree: an empty `fixtures/` that
            // vanished would be a different source.
            entries.insert((format!("{}/", relative.display()), String::new()));
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

    let mut hasher = Sha256::new();
    hasher.update(RESOLVER_CONTRACT_V1.as_bytes());
    hasher.update([0]);
    for (path, digest) in &entries {
        for field in [path.as_str(), digest.as_str()] {
            hasher.update((field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
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

    let mut tar = tar::Archive::new(Cursor::new(archive));
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
        let relative = safe_entry_path(&path, limits)?;
        let target = destination.join(&relative);

        if entry_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| {
                SourceError::Unusable(format!("cannot create {}: {error}", relative.display()))
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
    fn links_devices_and_fifos_are_refused() {
        // A source tree is files and directories. The rest are ways to reach
        // outside it once it lands on a disk.
        for entry_type in [
            tar::EntryType::Symlink,
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
}
