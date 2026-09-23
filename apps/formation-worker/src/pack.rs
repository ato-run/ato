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
//!
//! Three entry kinds: directories, regular files and contained relative
//! symlinks. A link is packed as a link — its target string, never the
//! content it points at — after checking it again here: a build writes its
//! own links, so a link being fine in the source says nothing about the built
//! tree. The rule is the one the source resolver uses
//! (`ato_formation::containment`). An absolute link (`/opt/…`, `/etc/…`) or
//! one that climbs out of the tree is refused: a dependency outside the
//! artifact is not something an artifact can carry by pointing at the build
//! host. A tree with no symlink packs to exactly the bytes it always did.
//!
//! Errors name tree-relative paths only: they reach the requester, and the
//! worker's scratch layout is not theirs.

use std::collections::{BTreeMap, BTreeSet};

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_formation::containment::validate_contained_symlink_target;

/// Pack `root` into a deterministic archive.
pub fn pack_tree(root: &Path) -> Result<Vec<u8>> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut links = BTreeMap::new();
    collect(root, root, &mut files, &mut directories, &mut links)?;
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
            let name = format!("{}/", utf8(relative, "directory")?);
            builder
                .append_data(&mut header, &name, std::io::empty())
                .with_context(|| format!("cannot pack directory {name}"))?;
        }
        for relative in &files {
            let absolute = root.join(relative);
            let name = utf8(relative, "file")?;
            let contents =
                std::fs::read(&absolute).with_context(|| format!("cannot read {name}"))?;
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
            builder
                .append_data(&mut header, name, Cursor::new(&contents))
                .with_context(|| format!("cannot pack {name}"))?;
        }
        // After everything else, so a tree without links is byte-for-byte
        // what it always was. GNU headers: a target may be longer than the
        // 100 bytes a ustar link field holds.
        for (relative, target) in &links {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Symlink);
            let name = utf8(relative, "link")?;
            builder
                .append_link(&mut header, name, target)
                .with_context(|| format!("cannot pack link {name}"))?;
        }
        builder.finish().context("cannot finish the archive")?;
    }
    Ok(bytes)
}

#[cfg(unix)]
fn os_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

fn utf8<'a>(relative: &'a Path, what: &str) -> Result<&'a str> {
    relative
        .to_str()
        .with_context(|| format!("a {what} name in the built tree is not UTF-8"))
}

fn collect(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
    directories: &mut BTreeSet<PathBuf>,
    links: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    let relative_of = |path: &Path| -> Result<PathBuf> {
        Ok(path
            .strip_prefix(root)
            .context("entry escaped the tree root")?
            .to_path_buf())
    };
    let here = relative_of(directory)?;
    for entry in
        std::fs::read_dir(directory).with_context(|| format!("cannot read {}/", here.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let relative = relative_of(&path)?;
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("cannot read {}", relative.display()))?;
        if metadata.is_symlink() {
            let target = std::fs::read_link(&path)
                .with_context(|| format!("cannot read the link {}", relative.display()))?;
            let target = validate_contained_symlink_target(&relative, &os_bytes(&target)).map_err(
                |escape| {
                    anyhow::anyhow!(
                        "built tree contains a symlink at {} -> {} that the artifact cannot \
                         carry: {}",
                        relative.display(),
                        target.display(),
                        escape.reason
                    )
                },
            )?;
            links.insert(relative, target);
            continue;
        }
        if metadata.is_dir() {
            directories.insert(relative);
            collect(root, &path, files, directories, links)?;
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
