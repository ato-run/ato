//! Safe materialization of a native artifact blob.
//!
//! The bundle's closure already proved the blob is the digest the descriptor
//! pinned. What this module adds is that unpacking it cannot write outside the
//! directory the Run owns: every entry is a relative, traversal-free regular
//! file or directory, links and devices are refused, and the executable bits
//! the author packed are preserved (with setuid/setgid/sticky stripped).
//!
//! The digest of what was actually placed is recorded as execution evidence.
//!
//! A `zip` payload is unpacked by the sibling `zip` module: it applies the same
//! rules to the entries its central directory lists, and a `dmg` still has no
//! extractor in this build.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use ato_formation::native_desktop::NativeArtifactFormat;
use sha2::{Digest, Sha256};

/// Total bytes a Run may materialize from one artifact.
pub const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;

/// Extract limits for one archive.
///
/// A runtime artifact is one program; a source tree is a whole repository at a
/// commit, carrying tests, docs and fixtures the build may not touch. The
/// source tree is therefore bounded far above the artifact cap, and still
/// bounded.
#[derive(Debug, Clone, Copy)]
pub struct ExtractLimits {
    pub max_bytes: u64,
    pub max_entries: u64,
}

pub const ARTIFACT_LIMITS: ExtractLimits = ExtractLimits {
    max_bytes: MAX_EXTRACTED_BYTES,
    max_entries: MAX_EXTRACTED_ENTRIES,
};

/// A source build unpacks the pinned commit, not one program.
pub const SOURCE_LIMITS: ExtractLimits = ExtractLimits {
    max_bytes: 4 * 1024 * 1024 * 1024,
    max_entries: 400_000,
};
/// Entries a Run may materialize from one artifact.
pub const MAX_EXTRACTED_ENTRIES: u64 = 100_000;

/// What extraction placed, and the digest of the placed tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedArtifact {
    pub root: PathBuf,
    /// `sha256:` over every placed file's path, mode, size and content digest.
    pub tree_digest: String,
    pub file_count: u64,
    pub total_bytes: u64,
}

/// Unpack a `tar.gz` artifact under `destination`, which must not exist yet.
pub fn extract_tar_gz(archive: &[u8], destination: &Path) -> Result<ExtractedArtifact> {
    extract_tar_reader(std::io::Cursor::new(archive), destination)
}

/// Unpack a `tar.gz` from any reader under `destination`.
pub fn extract_tar_reader<R: Read>(archive: R, destination: &Path) -> Result<ExtractedArtifact> {
    extract_tar_reader_with(archive, destination, ARTIFACT_LIMITS)
}

/// Unpack a source-build tarball: the same rules, with the source tree bounds.
pub fn extract_source_tar_gz(archive: &[u8], destination: &Path) -> Result<ExtractedArtifact> {
    extract_tar_reader_with(std::io::Cursor::new(archive), destination, SOURCE_LIMITS)
}

fn extract_tar_reader_with<R: Read>(
    archive: R,
    destination: &Path,
    limits: ExtractLimits,
) -> Result<ExtractedArtifact> {
    ensure!(
        !destination.exists(),
        "artifact destination already exists: {}",
        destination.display()
    );
    fs::create_dir_all(destination)
        .with_context(|| format!("create artifact root {}", destination.display()))?;

    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let mut files = Vec::new();
    let mut links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut entries = 0_u64;
    let mut total_bytes = 0_u64;

    for entry in tar
        .entries()
        .context("the native artifact is not a readable tar archive")?
    {
        let mut entry = entry.context("the native artifact entry is unreadable")?;
        entries += 1;
        ensure!(
            entries <= limits.max_entries,
            "the native artifact carries more than {} entries",
            limits.max_entries
        );
        let kind = entry.header().entry_type();
        let raw = entry
            .path()
            .context("a native artifact entry has no path")?
            .into_owned();
        let relative = safe_relative(&raw)
            .with_context(|| format!("native artifact entry `{}`", raw.display()))?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let mode = entry.header().mode().unwrap_or(0o644) & 0o777;
        match kind {
            tar::EntryType::Directory => {
                let directory = destination.join(&relative);
                fs::create_dir_all(&directory).with_context(|| {
                    format!("create artifact directory {}", directory.display())
                })?;
                set_mode(&directory, mode & 0o755)?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                ensure!(
                    seen.insert(relative.clone()),
                    "the native artifact carries `{}` more than once",
                    relative.display()
                );
                let size = entry
                    .header()
                    .size()
                    .context("a native artifact entry has no size")?;
                total_bytes = total_bytes.saturating_add(size);
                ensure!(
                    total_bytes <= limits.max_bytes,
                    "the native artifact expands past {} bytes",
                    limits.max_bytes
                );
                let output = destination.join(&relative);
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("create artifact directory {}", parent.display())
                    })?;
                }
                let mut file = File::create(&output)
                    .with_context(|| format!("create artifact file {}", output.display()))?;
                let mut hasher = Sha256::new();
                let mut written = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let read = entry
                        .read(&mut buffer)
                        .context("read a native artifact entry")?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                    std::io::Write::write_all(&mut file, &buffer[..read])
                        .with_context(|| format!("write artifact file {}", output.display()))?;
                    written += read as u64;
                }
                ensure!(
                    written == size,
                    "artifact entry `{}` is shorter than its declared size",
                    relative.display()
                );
                drop(file);
                set_mode(&output, mode)?;
                files.push(PlacedFile {
                    path: slash_path(&relative),
                    mode,
                    size: written,
                    digest: format!("{:x}", hasher.finalize()),
                });
            }
            // Pax and GNU metadata headers describe the entry that follows.
            // They carry no tree content of their own, and the tar reader
            // applies the fields it understands to that entry. GitHub's
            // codeload tarballs open with one of them.
            tar::EntryType::XHeader
            | tar::EntryType::XGlobalHeader
            | tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink => {}
            // A source archive may carry symlinks. They are resolved inside
            // the artifact root and placed as copies: a link that escapes the
            // root, or names nothing the archive carries, is refused rather
            // than materialized.
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()
                    .context("a native artifact symlink has no target")?
                    .map(|name| name.into_owned())
                    .ok_or_else(|| anyhow::anyhow!("a native artifact symlink has no target"))?;
                ensure!(
                    seen.insert(relative.clone()),
                    "the native artifact carries '{}' more than once",
                    relative.display()
                );
                links.push((relative, target));
            }
            other => bail!(
                "native artifact entry '{}' is a {other:?}; only regular files and directories are materialized",
                relative.display()
            ),
        }
    }
    // Resolve symlinks as copies. A chain is followed over bounded passes; a
    // target outside the artifact root, or one nothing materializes, fails.
    for _ in 0..8 {
        let pending = std::mem::take(&mut links);
        for (link, target) in pending {
            let Some(resolved) = resolve_inside(&link, &target) else {
                bail!(
                    "native artifact symlink '{}' points outside the artifact",
                    link.display()
                );
            };
            let source = destination.join(&resolved);
            if !source.is_file() {
                links.push((link, target));
                continue;
            }
            let bytes = fs::read(&source)
                .with_context(|| format!("read artifact symlink target {}", source.display()))?;
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            ensure!(
                total_bytes <= limits.max_bytes,
                "the native artifact expands past {} bytes",
                limits.max_bytes
            );
            let output = destination.join(&link);
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create artifact directory {}", parent.display()))?;
            }
            fs::write(&output, &bytes)
                .with_context(|| format!("write artifact symlink copy {}", output.display()))?;
            set_mode(&output, 0o644)?;
            files.push(PlacedFile {
                path: slash_path(&link),
                mode: 0o644,
                size: bytes.len() as u64,
                digest: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
        if links.is_empty() {
            break;
        }
    }
    ensure!(
        links.is_empty(),
        "native artifact symlinks name nothing the archive carries: {}",
        links
            .iter()
            .map(|(link, _)| link.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    ensure!(
        !files.is_empty(),
        "the native artifact carries no regular file"
    );
    finish(destination, files, total_bytes)
}

/// Resolve a symlink target relative to the directory holding `link`,
/// refusing any result outside the artifact root.
fn resolve_inside(link: &Path, target: &Path) -> Option<PathBuf> {
    if target.is_absolute() {
        return None;
    }
    let mut resolved = link.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in target.components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (!resolved.as_os_str().is_empty()).then_some(resolved)
}

/// Digest what was placed, in one canonical form.
pub(super) fn finish(
    destination: &Path,
    mut files: Vec<PlacedFile>,
    total_bytes: u64,
) -> Result<ExtractedArtifact> {
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut canonical = String::new();
    for file in &files {
        canonical.push_str(&file.path);
        canonical.push('\0');
        canonical.push_str(&format!("{:04o}", file.mode));
        canonical.push('\0');
        canonical.push_str(&file.size.to_string());
        canonical.push('\0');
        canonical.push_str(&file.digest);
        canonical.push('\n');
    }
    let tree_digest = format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()));
    Ok(ExtractedArtifact {
        root: destination.to_path_buf(),
        tree_digest,
        file_count: files.len() as u64,
        total_bytes,
    })
}

pub(super) struct PlacedFile {
    pub(super) path: String,
    pub(super) mode: u32,
    pub(super) size: u64,
    pub(super) digest: String,
}

/// A path the archive names, when it is safe to join under the root.
pub(super) fn safe_relative(path: &Path) -> Result<PathBuf> {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => {
                ensure!(
                    !segment.is_empty() && segment != "." && segment != "..",
                    "entry path contains an unsafe segment"
                );
                cleaned.push(segment);
            }
            Component::CurDir => {}
            Component::ParentDir => bail!("entry path traverses with `..`"),
            Component::RootDir | Component::Prefix(_) => bail!("entry path is absolute"),
        }
    }
    Ok(cleaned)
}

pub(super) fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(unix)]
pub(super) fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set mode {mode:04o} on {}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

/// Unpack a remote payload of the declared format under `destination`.
///
/// The format vocabulary belongs to the Adapter, so an unsupported packed form
/// is a named refusal here rather than a bundle-validation failure: the
/// declaration is well-formed, this Adapter just cannot unpack it. A `zip` is
/// unpacked by the sibling module under exactly the rules this one applies to a
/// `tar.gz`; a `dmg` still has no extractor in this build.
pub fn extract_archive(
    bytes: &[u8],
    format: NativeArtifactFormat,
    destination: &Path,
) -> Result<ExtractedArtifact> {
    match format {
        NativeArtifactFormat::TarGz => extract_tar_gz(bytes, destination),
        NativeArtifactFormat::Zip => super::zip::extract_zip(bytes, destination),
        NativeArtifactFormat::Dmg => bail!(
            "native_format_extractor_unavailable: this Adapter has no dmg extractor, so the \
             declared disk image cannot be materialized"
        ),
    }
}

/// Unpack a payload that is on disk, streaming it instead of holding it.
pub fn extract_archive_file(
    path: &Path,
    format: NativeArtifactFormat,
    destination: &Path,
) -> Result<ExtractedArtifact> {
    match format {
        NativeArtifactFormat::TarGz => {
            let file = File::open(path)
                .with_context(|| format!("open the verified payload {}", path.display()))?;
            extract_tar_reader(file, destination)
        }
        NativeArtifactFormat::Zip => super::zip::extract_zip_file(path, destination),
        // The Adapter owns the packed formats it can unpack. A disk image is a
        // well-formed declaration this build cannot materialize; saying so
        // keeps "declared" and "implemented" apart.
        NativeArtifactFormat::Dmg => bail!(
            "native_format_extractor_unavailable: this Adapter has no dmg extractor, so the \
             declared disk image cannot be materialized"
        ),
    }
}

/// Adopt a directory a source build produced, as the artifact tree.
///
/// The build wrote this tree in this Run, so nothing is unpacked; the digest is
/// still recomputed from the bytes on disk so the receipt names what was
/// actually launched.
pub fn adopt_tree(root: &Path) -> Result<ExtractedArtifact> {
    ensure!(
        root.is_dir(),
        "the built artifact root {} is not a directory",
        root.display()
    );
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read the built tree {}", directory.display()))?
        {
            let entry = entry.context("read a built-tree entry")?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect the built file {}", path.display()))?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(
                metadata.is_file(),
                "the built tree carries {}, which is not a regular file",
                path.display()
            );
            ensure!(
                (files.len() as u64) < MAX_EXTRACTED_ENTRIES,
                "the built tree carries more than {MAX_EXTRACTED_ENTRIES} entries"
            );
            let mode = built_mode(&metadata);
            let bytes = fs::read(&path)
                .with_context(|| format!("read the built file {}", path.display()))?;
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            ensure!(
                total_bytes <= MAX_EXTRACTED_BYTES,
                "the built tree is larger than {MAX_EXTRACTED_BYTES} bytes"
            );
            let relative = path.strip_prefix(root).with_context(|| {
                format!("the built file {} is outside its root", path.display())
            })?;
            files.push(PlacedFile {
                path: slash_path(relative),
                mode,
                size: bytes.len() as u64,
                digest: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
    }
    ensure!(!files.is_empty(), "the built tree carries no regular file");
    finish(root, files, total_bytes)
}

#[cfg(unix)]
fn built_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn built_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;

    enum Entry<'a> {
        File(&'a str, &'a [u8], u32),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
        Hardlink(&'a str, &'a str),
        Fifo(&'a str),
    }

    /// Write a header whose name is not normalized. `tar::Builder` refuses to
    /// produce a traversing name, and the point of these tests is reading an
    /// archive produced by someone else, so the name is written directly.
    fn append_raw(
        builder: &mut tar::Builder<GzEncoder<Vec<u8>>>,
        name: &str,
        bytes: &[u8],
        mode: u32,
        kind: tar::EntryType,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(mode);
        header.set_entry_type(kind);
        {
            let raw = header.as_mut_bytes();
            raw[..100].fill(0);
            raw[..name.len()].copy_from_slice(name.as_bytes());
            raw[148..156].fill(b' ');
            let sum: u32 = raw.iter().map(|byte| u32::from(*byte)).sum();
            let octal = format!("{sum:06o}\0 ");
            raw[148..156].copy_from_slice(octal.as_bytes());
        }
        builder
            .append(&header, std::io::Cursor::new(bytes.to_vec()))
            .unwrap();
    }

    fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for entry in entries {
            match entry {
                Entry::File(path, bytes, mode) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(bytes.len() as u64);
                    header.set_mode(*mode);
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, std::io::Cursor::new(*bytes))
                        .unwrap();
                }
                Entry::Dir(path) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, std::io::empty())
                        .unwrap();
                }
                Entry::Symlink(path, target) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(0o777);
                    header.set_entry_type(tar::EntryType::Symlink);
                    builder.append_link(&mut header, path, target).unwrap();
                }
                Entry::Hardlink(path, target) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(0o644);
                    header.set_entry_type(tar::EntryType::Link);
                    builder.append_link(&mut header, path, target).unwrap();
                }
                Entry::Fifo(path) => {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(0);
                    header.set_mode(0o644);
                    header.set_entry_type(tar::EntryType::Fifo);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, std::io::empty())
                        .unwrap();
                }
            }
        }
        let encoder = builder.into_inner().unwrap();
        let mut output = Vec::new();
        std::io::Write::write_all(&mut output, &encoder.finish().unwrap()).unwrap();
        output
    }

    fn raw_archive(entries: &[(&str, &[u8], u32, tar::EntryType)]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes, mode, kind) in entries {
            append_raw(&mut builder, name, bytes, *mode, *kind);
        }
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_regular_artifact_places_files_and_preserves_the_executable_bit() {
        let bytes = archive(&[
            Entry::Dir("app/"),
            Entry::File("app/run.sh", b"#!/bin/sh\nexit 0\n", 0o755),
            Entry::File("app/proof.txt", b"proof\n", 0o644),
        ]);
        let scratch = temp_root();
        let root = scratch.path().join("artifact");
        let extracted = extract_tar_gz(&bytes, &root).unwrap();
        assert_eq!(extracted.file_count, 2);
        assert_eq!(extracted.total_bytes, 23);
        assert_eq!(fs::read(root.join("app/proof.txt")).unwrap(), b"proof\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join("app/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755);
        }
        assert!(extracted.tree_digest.starts_with("sha256:"));
    }

    #[test]
    fn the_same_archive_places_the_same_tree_digest_and_a_changed_one_does_not() {
        let first = archive(&[
            Entry::File("app/run.sh", b"#!/bin/sh\nexit 0\n", 0o755),
            Entry::File("app/proof.txt", b"proof\n", 0o644),
        ]);
        let second = archive(&[
            Entry::File("app/proof.txt", b"proof\n", 0o644),
            Entry::File("app/run.sh", b"#!/bin/sh\nexit 0\n", 0o755),
        ]);
        let changed = archive(&[
            Entry::File("app/run.sh", b"#!/bin/sh\nexit 1\n", 0o755),
            Entry::File("app/proof.txt", b"proof\n", 0o644),
        ]);
        let scratch = temp_root();
        let a = extract_tar_gz(&first, &scratch.path().join("a")).unwrap();
        let b = extract_tar_gz(&second, &scratch.path().join("b")).unwrap();
        let c = extract_tar_gz(&changed, &scratch.path().join("c")).unwrap();
        assert_eq!(a.tree_digest, b.tree_digest);
        assert_ne!(a.tree_digest, c.tree_digest);
    }

    #[test]
    fn a_mode_change_is_part_of_the_materialized_digest() {
        let executable = archive(&[Entry::File("app/run.sh", b"x", 0o755)]);
        let plain = archive(&[Entry::File("app/run.sh", b"x", 0o644)]);
        let scratch = temp_root();
        let a = extract_tar_gz(&executable, &scratch.path().join("a")).unwrap();
        let b = extract_tar_gz(&plain, &scratch.path().join("b")).unwrap();
        assert_ne!(a.tree_digest, b.tree_digest);
    }

    #[test]
    fn escaping_or_linked_entries_are_refused_before_placement() {
        for (what, bytes) in [
            (
                "parent traversal",
                raw_archive(&[("../evil", b"x", 0o644, tar::EntryType::Regular)]),
            ),
            (
                "nested traversal",
                raw_archive(&[("app/../../evil", b"x", 0o644, tar::EntryType::Regular)]),
            ),
            (
                "absolute path",
                raw_archive(&[("/etc/evil", b"x", 0o644, tar::EntryType::Regular)]),
            ),
            (
                "symlink",
                archive(&[Entry::Symlink("app/link", "/etc/passwd")]),
            ),
            (
                "relative symlink",
                archive(&[Entry::Symlink("app/link", "../../outside")]),
            ),
            (
                "hardlink",
                archive(&[Entry::Hardlink("app/hard", "app/real")]),
            ),
            ("fifo", archive(&[Entry::Fifo("app/pipe")])),
        ] {
            let scratch = temp_root();
            let root = scratch.path().join("artifact");
            let error = extract_tar_gz(&bytes, &root).unwrap_err();
            assert!(
                !root.join("evil").exists() && !root.join("app/evil").exists(),
                "{what}: an escaping entry was placed"
            );
            assert!(
                error.to_string().to_lowercase().contains("artifact")
                    || error.to_string().contains("entry"),
                "{what}: unexpected error `{error}`"
            );
        }
    }

    #[test]
    fn a_duplicate_entry_and_an_empty_artifact_are_refused() {
        let duplicate = archive(&[
            Entry::File("app/run", b"a", 0o644),
            Entry::File("app/run", b"b", 0o644),
        ]);
        let scratch = temp_root();
        assert!(extract_tar_gz(&duplicate, &scratch.path().join("a")).is_err());

        let empty = archive(&[Entry::Dir("app/")]);
        assert!(extract_tar_gz(&empty, &scratch.path().join("b")).is_err());

        let not_an_archive = b"not a gzip";
        assert!(extract_tar_gz(not_an_archive, &scratch.path().join("c")).is_err());
    }

    #[test]
    fn a_payload_on_disk_is_streamed_and_a_dmg_is_named_as_unavailable() {
        let bytes = archive(&[Entry::File("app/run.sh", b"#!/bin/sh\n", 0o755)]);
        let scratch = temp_root();
        let payload = scratch.path().join("artifact.tar.gz");
        fs::write(&payload, &bytes).unwrap();
        let extracted = extract_archive_file(
            &payload,
            ato_formation::native_desktop::NativeArtifactFormat::TarGz,
            &scratch.path().join("disk"),
        )
        .unwrap();
        assert_eq!(extracted.file_count, 1);

        // The disk image is the one packed form with no extractor in this
        // build; a zip is unpacked by the zip module and has its own tests.
        let format = ato_formation::native_desktop::NativeArtifactFormat::Dmg;
        for error in [
            extract_archive_file(&payload, format, &scratch.path().join("file")).unwrap_err(),
            extract_archive(&bytes, format, &scratch.path().join("memory")).unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .starts_with("native_format_extractor_unavailable"),
                "{format:?}: {error}"
            );
        }
    }

    #[test]
    fn a_built_tree_is_adopted_with_its_own_digest_and_nothing_else() {
        let scratch = temp_root();
        let root = scratch.path().join("staging");
        let release = root.join(".ato-build/target/release");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("app"), b"built binary").unwrap();
        let adopted = adopt_tree(&root).unwrap();
        assert_eq!(adopted.file_count, 1);
        assert_eq!(adopted.total_bytes, 12);
        assert_eq!(adopted.root, root);
        assert_eq!(
            adopted.tree_digest,
            adopt_tree(&root).unwrap().tree_digest,
            "the same tree digests the same way"
        );

        // A change to the built bytes changes the digest the receipt records.
        fs::write(release.join("app"), b"other binary").unwrap();
        assert_ne!(adopted.tree_digest, adopt_tree(&root).unwrap().tree_digest);

        // A link is not a regular file, and an empty tree is not an artifact.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", release.join("link")).unwrap();
            assert!(adopt_tree(&root).is_err());
            fs::remove_file(release.join("link")).unwrap();
        }
        let empty = scratch.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(adopt_tree(&empty).is_err());
    }

    #[test]
    fn a_destination_that_exists_is_never_overwritten() {
        let bytes = archive(&[Entry::File("app/run", b"a", 0o644)]);
        let scratch = temp_root();
        let root = scratch.path().join("artifact");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("keep"), b"keep").unwrap();
        assert!(extract_tar_gz(&bytes, &root).is_err());
        assert_eq!(fs::read(root.join("keep")).unwrap(), b"keep");
    }
}

#[cfg(test)]
mod symlink_tests {
    use super::*;

    fn archive_with_link(target: &str) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, "root/doc.md", &b"hello"[..])
                .unwrap();
            let mut link = tar::Header::new_gnu();
            link.set_size(0);
            link.set_mode(0o777);
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_link_name(target).unwrap();
            link.set_cksum();
            builder
                .append_data(&mut link, "root/link.md", &[][..])
                .unwrap();
            builder.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    #[test]
    fn a_symlink_inside_the_archive_is_placed_as_a_copy() {
        let archive = archive_with_link("doc.md");
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("artifact");
        let extracted = extract_tar_gz(&archive, &root).expect("extract");
        assert_eq!(fs::read(root.join("root/link.md")).unwrap(), b"hello");
        assert_eq!(extracted.file_count, 2);
    }

    #[test]
    fn a_symlink_that_escapes_the_artifact_is_refused() {
        let archive = archive_with_link("../../outside");
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("artifact");
        assert!(extract_tar_gz(&archive, &root).is_err());
    }
}
