//! Safe materialization of a native artifact blob.
//!
//! The bundle's closure already proved the blob is the digest the descriptor
//! pinned. What this module adds is that unpacking it cannot write outside the
//! directory the Run owns: every entry is a relative, traversal-free regular
//! file or directory, links and devices are refused, and the executable bits
//! the author packed are preserved (with setuid/setgid/sticky stripped).
//!
//! The digest of what was actually placed is recorded as execution evidence.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

/// Total bytes a Run may materialize from one artifact.
pub const MAX_EXTRACTED_BYTES: u64 = 512 * 1024 * 1024;
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
            entries <= MAX_EXTRACTED_ENTRIES,
            "the native artifact carries more than {MAX_EXTRACTED_ENTRIES} entries"
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
                    total_bytes <= MAX_EXTRACTED_BYTES,
                    "the native artifact expands past {MAX_EXTRACTED_BYTES} bytes"
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
            other => bail!(
                "native artifact entry `{}` is a {other:?}; only regular files and directories are materialized",
                relative.display()
            ),
        }
    }
    ensure!(
        !files.is_empty(),
        "the native artifact carries no regular file"
    );
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

struct PlacedFile {
    path: String,
    mode: u32,
    size: u64,
    digest: String,
}

/// A path the archive names, when it is safe to join under the root.
fn safe_relative(path: &Path) -> Result<PathBuf> {
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

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set mode {mode:04o} on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
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
