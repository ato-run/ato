//! Deterministic source-archive materialization for the `source_materialize`
//! builder job (`docs/rfcs/draft/SOURCE_MATERIALIZATION_SPEC.md`).
//!
//! [`materialize_source_archive`] turns a materialized checkout into a frozen,
//! content-addressed `.tar.zst` plus its A1v2 identity, in this order:
//!
//! 1. Run the A1v2 admissibility profile + hash
//!    ([`super::source_tree::materialized_source_tree_hash`]). A tree that
//!    violates any admissibility rule (symlink, submodule, LFS pointer, per-file
//!    or file-count cap, …) has **no** archive: the function returns before a
//!    single byte is written.
//! 2. Build a **deterministic** `tar` of the admissible tree — entries in A1's
//!    per-directory raw-byte-sorted depth-first order, headers normalized
//!    (mode `0o755` if the A1 executable bit is set else `0o644`; `mtime`/`uid`/
//!    `gid` = 0; no uname/gname; no xattrs) — so two builders produce the same
//!    archive bytes for the same tree.
//! 3. Compress single-threaded at a fixed zstd level (never the `zstdmt` path,
//!    whose output depends on the worker count) and hash the exact `.tar.zst`
//!    bytes with [`super::source_tree::source_archive_hash`].
//! 4. Enforce the archive-level caps the tree walk does not cover — compressed
//!    `<= 100 MiB`, uncompressed `<= 250 MiB` (the per-file 50 MiB and 50k-file
//!    caps are already enforced inside `materialized_source_tree_hash`).
//!
//! The `source_archive_hash` is only guaranteed reproducible across builders
//! running the SAME binary (same `tar`/`zstd` versions) — it is the byte
//! identity of *this* encoding, not a version-independent tree identity. The
//! version-independent identity is `materialized_source_tree_hash`; two valid
//! encodings of one tree share it but differ in `source_archive_hash` (see the
//! `source_archive_hash` doc comment).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tar::{Builder, EntryType, Header};
use thiserror::Error;

use super::source_tree::{
    SourceAdmissibilityError, materialized_source_tree_hash, source_archive_hash,
};

/// Production cap on the compressed `.tar.zst` (100 MiB); exceeding it blocks the
/// repo (SOURCE_MATERIALIZATION_SPEC §3.4).
pub const MAX_COMPRESSED_BYTES: u64 = 100 * 1024 * 1024;

/// Production cap on the uncompressed `tar` stream (250 MiB); exceeding it blocks
/// the repo (SOURCE_MATERIALIZATION_SPEC §3.4). The per-file (50 MiB) and
/// file-count ([`super::source_tree::MAX_FILE_COUNT`]) caps are enforced earlier,
/// inside [`materialized_source_tree_hash`]; this bounds the aggregate they do not.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 250 * 1024 * 1024;

/// zstd compression level. Fixed and single-threaded ([`zstd::encode_all`], not
/// the `zstdmt` multi-worker path) so a given `tar` compresses to identical bytes
/// on any builder running this binary — the archive is content-addressed by
/// `source_archive_hash`, so its bytes must be reproducible.
const ZSTD_LEVEL: i32 = 19;

/// The frozen-source result: the A1v2 identity, the archive byte identity, and
/// the observed sizes/count. Every field is non-secret build provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedSource {
    /// A1v2 `sha256:<hex>` identity of the admissible tree
    /// ([`materialized_source_tree_hash`]).
    pub materialized_source_tree_hash: String,
    /// `sha256:<hex>` of the exact `.tar.zst` bytes ([`source_archive_hash`]).
    pub source_archive_hash: String,
    /// Number of regular files archived.
    pub file_count: u64,
    /// Size of the uncompressed `tar` stream in bytes.
    pub uncompressed_bytes: u64,
    /// Size of the compressed `.tar.zst` in bytes.
    pub compressed_bytes: u64,
}

/// Why a checkout could not be materialized into a frozen source archive.
///
/// Each variant carries the pipeline state it maps to via [`Self::pipeline_state`]:
/// admissibility / cap violations are terminal `blocked_repo`; an
/// [`Io`](Self::Io) (or a delegated [`SourceAdmissibilityError::Io`]) is a
/// transient `failed_internal` the job may retry.
#[derive(Debug, Error)]
pub enum SourceMaterializeError {
    /// The tree failed the A1v2 admissibility profile — no archive was written.
    #[error("source tree is not admissible: {0}")]
    Inadmissible(#[from] SourceAdmissibilityError),

    /// The uncompressed `tar` stream exceeds [`MAX_UNCOMPRESSED_BYTES`].
    #[error("uncompressed archive is {bytes} bytes, over the {limit}-byte cap")]
    UncompressedTooLarge { bytes: u64, limit: u64 },

    /// The compressed `.tar.zst` exceeds [`MAX_COMPRESSED_BYTES`].
    #[error("compressed archive is {bytes} bytes, over the {limit}-byte cap")]
    CompressedTooLarge { bytes: u64, limit: u64 },

    /// An I/O / archive-encoding error occurred while reading the tree, writing
    /// the `tar`, or compressing.
    #[error("io error {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl SourceMaterializeError {
    /// The GitHub-capsule-request pipeline state this failure maps to
    /// (SOURCE_MATERIALIZATION_SPEC §4.2). Admissibility / cap violations are the
    /// terminal `blocked_repo`; an admissibility [`Io`](SourceAdmissibilityError::Io)
    /// or an archive-side [`Io`](Self::Io) is a retryable `failed_internal`.
    pub fn pipeline_state(&self) -> &'static str {
        match self {
            SourceMaterializeError::Inadmissible(e) => e.pipeline_state(),
            SourceMaterializeError::UncompressedTooLarge { .. }
            | SourceMaterializeError::CompressedTooLarge { .. } => "blocked_repo",
            SourceMaterializeError::Io { .. } => "failed_internal",
        }
    }

    /// A short, stable machine code identifying the failure class — the ack's
    /// `error_code` field, kept separate from the human-readable detail so the
    /// pipeline can branch on it without string-matching a message.
    pub fn error_code(&self) -> &'static str {
        match self {
            SourceMaterializeError::Inadmissible(_) => "inadmissible_source_tree",
            SourceMaterializeError::UncompressedTooLarge { .. } => "uncompressed_cap_exceeded",
            SourceMaterializeError::CompressedTooLarge { .. } => "compressed_cap_exceeded",
            SourceMaterializeError::Io { .. } => "io_error",
        }
    }
}

/// Archive-level caps applied after the tree walk. Production always uses
/// [`ArchiveCaps::PRODUCTION`]; the field-taking form exists only so in-crate
/// tests can exercise the caps with tiny thresholds instead of materializing a
/// 100 / 250 MiB archive. There is no public API that lowers the production caps.
#[derive(Debug, Clone, Copy)]
struct ArchiveCaps {
    max_uncompressed: u64,
    max_compressed: u64,
}

impl ArchiveCaps {
    const PRODUCTION: ArchiveCaps = ArchiveCaps {
        max_uncompressed: MAX_UNCOMPRESSED_BYTES,
        max_compressed: MAX_COMPRESSED_BYTES,
    };
}

/// Materialize `checkout_root` into a deterministic, content-addressed
/// `.tar.zst` at `out_tar_zst`, returning its A1v2 identity + observed sizes.
///
/// Runs the A1v2 admissibility profile + hash FIRST; an inadmissible tree
/// returns a [`SourceMaterializeError::Inadmissible`] before any archive byte is
/// written. See the module docs for the full ordering and the determinism
/// guarantee.
pub fn materialize_source_archive(
    checkout_root: &Path,
    out_tar_zst: &Path,
) -> Result<MaterializedSource, SourceMaterializeError> {
    materialize_source_archive_with_caps(checkout_root, out_tar_zst, &ArchiveCaps::PRODUCTION)
}

fn materialize_source_archive_with_caps(
    checkout_root: &Path,
    out_tar_zst: &Path,
    caps: &ArchiveCaps,
) -> Result<MaterializedSource, SourceMaterializeError> {
    // (a) Admissibility + A1v2 identity. `?` maps a SourceAdmissibilityError into
    // `Inadmissible` (which forwards its pipeline_state), so an inadmissible tree
    // stops here — before the tree is walked for archiving or `out_tar_zst` is
    // touched.
    let tree_hash = materialized_source_tree_hash(checkout_root)?;

    // (b) Collect entries in A1's per-directory raw-byte-sorted DFS order and sum
    // regular-file content so a hostile tree cannot force an unbounded in-memory
    // `tar` buffer before the cap check (per-file / file-count caps already ran).
    let mut entries: Vec<ArchiveEntry> = Vec::new();
    let mut content_bytes: u64 = 0;
    let mut file_count: u64 = 0;
    collect_entries(
        checkout_root,
        checkout_root,
        &mut entries,
        &mut content_bytes,
        &mut file_count,
    )?;
    if content_bytes > caps.max_uncompressed {
        return Err(SourceMaterializeError::UncompressedTooLarge {
            bytes: content_bytes,
            limit: caps.max_uncompressed,
        });
    }

    // Build the deterministic tar in memory. Bounded above by content_bytes (<=
    // cap) plus per-entry header/padding overhead.
    let tar_bytes = build_deterministic_tar(&entries)?;
    let uncompressed_bytes = tar_bytes.len() as u64;
    if uncompressed_bytes > caps.max_uncompressed {
        return Err(SourceMaterializeError::UncompressedTooLarge {
            bytes: uncompressed_bytes,
            limit: caps.max_uncompressed,
        });
    }

    // (c) Compress single-threaded at a fixed level so the bytes are reproducible.
    let compressed = zstd::encode_all(tar_bytes.as_slice(), ZSTD_LEVEL).map_err(|source| {
        SourceMaterializeError::Io {
            context: "zstd compress source tar".to_string(),
            source,
        }
    })?;
    let compressed_bytes = compressed.len() as u64;

    // (d) Enforce the compressed cap BEFORE writing — a cap violation leaves no
    // archive on disk, same as an inadmissible tree.
    if compressed_bytes > caps.max_compressed {
        return Err(SourceMaterializeError::CompressedTooLarge {
            bytes: compressed_bytes,
            limit: caps.max_compressed,
        });
    }

    if let Some(parent) = out_tar_zst.parent() {
        fs::create_dir_all(parent).map_err(|source| SourceMaterializeError::Io {
            context: format!("create archive directory {}", parent.display()),
            source,
        })?;
    }
    fs::write(out_tar_zst, &compressed).map_err(|source| SourceMaterializeError::Io {
        context: format!("write archive {}", out_tar_zst.display()),
        source,
    })?;

    // The archive hash is sha256 over the exact bytes just written; the in-memory
    // `compressed` buffer is byte-identical to the file, so hash it directly.
    let archive_hash = source_archive_hash(&compressed);

    Ok(MaterializedSource {
        materialized_source_tree_hash: tree_hash,
        source_archive_hash: archive_hash,
        file_count,
        uncompressed_bytes,
        compressed_bytes,
    })
}

/// One entry to archive: a regular file or a directory, with its path relative to
/// the checkout root. Symlinks / devices / etc. never appear — admissibility ran
/// first and rejected them.
struct ArchiveEntry {
    /// Path relative to the checkout root (POSIX separators on the Linux builder).
    rel: PathBuf,
    /// Absolute path used to read the file contents.
    abs: PathBuf,
    kind: EntryKind,
}

enum EntryKind {
    Dir,
    File { size: u64, executable: bool },
}

/// Walk `dir` recursively in A1's order (each directory's children sorted by raw
/// name bytes, depth-first), pushing a directory entry before recursing into it.
/// Mirrors [`super::tree_hash`]'s `read_dir_sorted` so the archive order tracks
/// the fold order the identity is computed in.
fn collect_entries(
    root: &Path,
    dir: &Path,
    out: &mut Vec<ArchiveEntry>,
    content_bytes: &mut u64,
    file_count: &mut u64,
) -> Result<(), SourceMaterializeError> {
    let read = fs::read_dir(dir).map_err(|source| SourceMaterializeError::Io {
        context: format!("read dir {}", dir.display()),
        source,
    })?;
    let mut children: Vec<(Vec<u8>, PathBuf)> = Vec::new();
    for entry in read {
        let entry = entry.map_err(|source| SourceMaterializeError::Io {
            context: format!("read dir entry under {}", dir.display()),
            source,
        })?;
        let path = entry.path();
        children.push((raw_name_bytes(&entry.file_name()), path));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));

    for (_, path) in children {
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| SourceMaterializeError::Io {
                context: format!("stat {}", path.display()),
                source,
            })?;
        let file_type = metadata.file_type();
        let rel = path
            .strip_prefix(root)
            .map_err(|_| SourceMaterializeError::Io {
                context: format!(
                    "path {} is not under root {}",
                    path.display(),
                    root.display()
                ),
                source: io::Error::new(io::ErrorKind::InvalidInput, "path escapes root"),
            })?
            .to_path_buf();

        if file_type.is_dir() {
            out.push(ArchiveEntry {
                rel,
                abs: path.clone(),
                kind: EntryKind::Dir,
            });
            collect_entries(root, &path, out, content_bytes, file_count)?;
        } else if file_type.is_file() {
            let size = metadata.len();
            *content_bytes = content_bytes.saturating_add(size);
            *file_count += 1;
            out.push(ArchiveEntry {
                rel,
                abs: path,
                kind: EntryKind::File {
                    size,
                    executable: is_executable(&metadata),
                },
            });
        } else {
            // Admissibility already ran and rejected symlinks / devices / sockets /
            // FIFOs; a non-file, non-dir entry here is a TOCTOU race (the tree
            // changed between the two walks) — a transient internal failure.
            return Err(SourceMaterializeError::Io {
                context: format!(
                    "unexpected node type at {} after admissibility",
                    path.display()
                ),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-file/non-dir entry after admissibility passed",
                ),
            });
        }
    }
    Ok(())
}

/// Serialize `entries` into a deterministic `tar` byte buffer. Every header field
/// is set explicitly (never copied from filesystem metadata) so the bytes depend
/// only on the entry's path, kind, size, and A1 executable bit.
fn build_deterministic_tar(entries: &[ArchiveEntry]) -> Result<Vec<u8>, SourceMaterializeError> {
    let mut builder = Builder::new(Vec::new());
    for entry in entries {
        let mut header = Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        // new_gnu() leaves uname/gname empty and device fields zero; we never copy
        // fs metadata, so those stay normalized.
        match entry.kind {
            EntryKind::Dir => {
                header.set_entry_type(EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                let mut name = entry.rel.to_string_lossy().into_owned();
                name.push('/'); // directory entries carry a trailing slash
                builder
                    .append_data(&mut header, &name, io::empty())
                    .map_err(|source| SourceMaterializeError::Io {
                        context: format!("append dir {} to tar", entry.rel.display()),
                        source,
                    })?;
            }
            EntryKind::File { size, executable } => {
                header.set_entry_type(EntryType::Regular);
                header.set_mode(if executable { 0o755 } else { 0o644 });
                header.set_size(size);
                let file =
                    fs::File::open(&entry.abs).map_err(|source| SourceMaterializeError::Io {
                        context: format!("open {} for archiving", entry.abs.display()),
                        source,
                    })?;
                builder
                    .append_data(&mut header, &entry.rel, file)
                    .map_err(|source| SourceMaterializeError::Io {
                        context: format!("append file {} to tar", entry.rel.display()),
                        source,
                    })?;
            }
        }
    }
    builder
        .into_inner()
        .map_err(|source| SourceMaterializeError::Io {
            context: "finalize tar".to_string(),
            source,
        })
}

#[cfg(unix)]
fn raw_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn raw_name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    // Admissibility already required UTF-8 names, so this is lossless in practice.
    name.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Owner executable bit only — the exact bit A1's tree hash folds in
    // (tree_hash::is_executable), so a `0o755`/`0o644` normalization preserves it.
    (metadata.permissions().mode() & 0o100) != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    // Matches A1: without POSIX permissions all files are non-executable.
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn write_file(root: &Path, rel: &str, contents: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    /// A small admissible tree used across the determinism / no-drift tests.
    fn build_sample_tree(root: &Path) {
        write_file(root, "README.md", b"hello\n");
        write_file(root, "src/main.rs", b"fn main() {}\n");
        write_file(root, "src/lib.rs", b"pub fn x() {}\n");
        write_file(root, "assets/logo.txt", b"art");
    }

    #[test]
    fn determinism_same_tree_twice_is_byte_identical() {
        let tree = tempfile::tempdir().unwrap();
        build_sample_tree(tree.path());
        let out_dir = tempfile::tempdir().unwrap();

        let first =
            materialize_source_archive(tree.path(), &out_dir.path().join("a.tar.zst")).unwrap();
        let second =
            materialize_source_archive(tree.path(), &out_dir.path().join("b.tar.zst")).unwrap();

        // The whole result — both hashes and every observed size — is reproducible.
        assert_eq!(first, second);
        assert_eq!(first.source_archive_hash, second.source_archive_hash);
        assert_eq!(
            first.materialized_source_tree_hash,
            second.materialized_source_tree_hash
        );
        // And the two archive files are byte-for-byte identical on disk.
        let a = fs::read(out_dir.path().join("a.tar.zst")).unwrap();
        let b = fs::read(out_dir.path().join("b.tar.zst")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn tree_hash_matches_the_a1v2_module_directly_no_drift() {
        let tree = tempfile::tempdir().unwrap();
        build_sample_tree(tree.path());
        let out = tempfile::tempdir().unwrap().path().join("src.tar.zst");

        let produced = materialize_source_archive(tree.path(), &out).unwrap();
        let direct = materialized_source_tree_hash(tree.path()).unwrap();
        assert_eq!(
            produced.materialized_source_tree_hash, direct,
            "the archive path must report the same A1v2 hash as the module directly"
        );
        assert_eq!(produced.file_count, 4);
    }

    #[cfg(unix)]
    #[test]
    fn inadmissible_symlink_blocks_before_any_archive_is_written() {
        use std::os::unix::fs::symlink;

        let tree = tempfile::tempdir().unwrap();
        write_file(tree.path(), "real.txt", b"target");
        symlink("real.txt", tree.path().join("link")).unwrap();

        let out = tempfile::tempdir().unwrap().path().join("blocked.tar.zst");
        let err = materialize_source_archive(tree.path(), &out).unwrap_err();

        assert!(
            matches!(
                err,
                SourceMaterializeError::Inadmissible(SourceAdmissibilityError::Symlink { .. })
            ),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
        assert_eq!(err.error_code(), "inadmissible_source_tree");
        assert!(
            !out.exists(),
            "no archive may be written for an inadmissible tree"
        );
    }

    #[test]
    fn archive_round_trips_to_the_same_tree_hash() {
        // Unpack the produced archive into a fresh dir and re-hash it: the frozen
        // archive must faithfully represent the tree it was made from (content +
        // names + executable bit all survive), so the A1v2 hash is unchanged.
        let tree = tempfile::tempdir().unwrap();
        build_sample_tree(tree.path());
        let out = tempfile::tempdir().unwrap().path().join("rt.tar.zst");
        let produced = materialize_source_archive(tree.path(), &out).unwrap();

        let compressed = fs::read(&out).unwrap();
        let mut tar_bytes = Vec::new();
        zstd::Decoder::new(compressed.as_slice())
            .unwrap()
            .read_to_end(&mut tar_bytes)
            .unwrap();

        let unpacked = tempfile::tempdir().unwrap();
        tar::Archive::new(tar_bytes.as_slice())
            .unpack(unpacked.path())
            .unwrap();

        let round_tripped = materialized_source_tree_hash(unpacked.path()).unwrap();
        assert_eq!(round_tripped, produced.materialized_source_tree_hash);
    }

    #[test]
    fn uncompressed_cap_exceeded_blocks_and_writes_nothing() {
        // A tiny uncompressed cap that even a single-file tar (>= one 512-byte
        // header + trailer) exceeds. The content pre-check passes (6 bytes), so the
        // POST-build uncompressed cap check is what fires.
        let tree = tempfile::tempdir().unwrap();
        write_file(tree.path(), "a.txt", b"hello\n");
        let out = tempfile::tempdir().unwrap().path().join("capped.tar.zst");
        let caps = ArchiveCaps {
            max_uncompressed: 8,
            max_compressed: MAX_COMPRESSED_BYTES,
        };

        let err = materialize_source_archive_with_caps(tree.path(), &out, &caps).unwrap_err();
        assert!(
            matches!(
                err,
                SourceMaterializeError::UncompressedTooLarge { limit: 8, .. }
            ),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
        assert_eq!(err.error_code(), "uncompressed_cap_exceeded");
        assert!(!out.exists(), "no archive on a cap failure");
    }

    #[test]
    fn compressed_cap_exceeded_blocks_and_writes_nothing() {
        let tree = tempfile::tempdir().unwrap();
        build_sample_tree(tree.path());
        let out = tempfile::tempdir().unwrap().path().join("capped2.tar.zst");
        let caps = ArchiveCaps {
            max_uncompressed: MAX_UNCOMPRESSED_BYTES,
            max_compressed: 1, // any real archive compresses to > 1 byte
        };

        let err = materialize_source_archive_with_caps(tree.path(), &out, &caps).unwrap_err();
        assert!(
            matches!(
                err,
                SourceMaterializeError::CompressedTooLarge { limit: 1, .. }
            ),
            "got {err:?}"
        );
        assert_eq!(err.pipeline_state(), "blocked_repo");
        assert_eq!(err.error_code(), "compressed_cap_exceeded");
        assert!(!out.exists(), "no archive on a cap failure");
    }

    #[test]
    fn production_caps_are_100mib_and_250mib() {
        use super::super::source_tree::{MAX_FILE_COUNT, MAX_FILE_SIZE_BYTES};
        // Guard against accidentally shipping the tiny test thresholds.
        assert_eq!(ArchiveCaps::PRODUCTION.max_compressed, 100 * 1024 * 1024);
        assert_eq!(ArchiveCaps::PRODUCTION.max_uncompressed, 250 * 1024 * 1024);
        // Keep the archive caps aligned with the tree-walk caps they extend.
        assert_eq!(MAX_FILE_COUNT, 50_000);
        assert_eq!(MAX_FILE_SIZE_BYTES, 50 * 1024 * 1024);
    }
}
