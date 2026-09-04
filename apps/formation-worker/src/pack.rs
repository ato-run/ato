//! Packing a built tree into the artifact the Runner materializes from.
//!
//! Deterministic, because the digest IS the artifact's identity: two builds of
//! the same tree must produce the same address, or nothing downstream can
//! coalesce or compare them.
//!
//! Entry order is sorted, and mtime, uid and gid are normalized away — none of
//! them are the build's output, and leaving them in would mint a new artifact
//! on every rebuild of unchanged code. The mode is reduced to one bit: whether
//! the owner may execute. That bit matters and the rest do not; a workspace
//! whose interpreter arrives non-executable does not start.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Pack `root` into a deterministic archive.
pub fn pack_tree(root: &Path) -> Result<Vec<u8>> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect(root, root, &mut files, &mut directories)?;

    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        // Directories are recorded, including empty ones: a `static/` that
        // vanished between build and run is a different artifact.
        for relative in &directories {
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Directory);
            let name = format!(
                "{}/",
                relative.to_str().context("directory name is not UTF-8")?
            );
            builder
                .append_data(&mut header, &name, std::io::empty())
                .with_context(|| format!("cannot pack directory {name}"))?;
        }
        for relative in &files {
            let absolute = root.join(relative);
            let contents = std::fs::read(&absolute)
                .with_context(|| format!("cannot read {}", relative.display()))?;
            let mut header = tar::Header::new_ustar();
            header.set_size(contents.len() as u64);
            header.set_mode(if is_owner_executable(&absolute)? {
                0o755
            } else {
                0o644
            });
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            let name = relative.to_str().context("file name is not UTF-8")?;
            builder
                .append_data(&mut header, name, Cursor::new(&contents))
                .with_context(|| format!("cannot pack {name}"))?;
        }
        builder.finish().context("cannot finish the archive")?;
    }
    Ok(bytes)
}

fn collect(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("cannot read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.is_symlink() {
            // The artifact format has no symlink entry, and following one would
            // silently duplicate its target — a venv's `python3` link becoming
            // a second copy of the interpreter. Refused so the build fails
            // where the link is, not where it is later missed.
            bail!(
                "built tree contains a symlink at {}, which the artifact format does not carry",
                path.display()
            );
        }
        let relative = path
            .strip_prefix(root)
            .context("entry escaped the tree root")?
            .to_path_buf();
        if metadata.is_dir() {
            directories.insert(relative);
            collect(root, &path, files, directories)?;
        } else if metadata.is_file() {
            files.insert(relative);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_owner_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o100 != 0)
}

#[cfg(not(unix))]
fn is_owner_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}
