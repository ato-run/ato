//! What the guest filesystem CONTAINS, as one digest.
//!
//! `filesystem.view_digest` used to be blake3 over the packed ext4. That was
//! measured, and wrong in a way only running it twice revealed: `mke2fs`
//! stamps every inode it creates with the wall clock and ignores
//! `SOURCE_DATE_EPOCH` (measured on e2fsprogs 1.47.0 — two packs ten seconds
//! apart differed in ~9,400 timestamp fields, while the UUID, the directory
//! hash seed and the superblock clocks were already pinned). Two builds of one
//! program source were therefore two different executions.
//!
//! Chasing byte-identity was possible — rewrite every inode's four timestamps
//! through `debugfs` — but it would have made the Execution Identity hostage to
//! e2fsprogs: an `apt upgrade` on a builder that changed block allocation would
//! change every capsule's `execution_id`, silently, with no source change. An
//! identity with that property is not one.
//!
//! So the contract commits the CONTENT instead. Two ext4 images holding the
//! same files, permissions and symlinks are the same execution however the
//! allocator laid them out, which is also the answer that matches what anyone
//! means by "the same guest".
//!
//! # What is committed, and what is deliberately not
//!
//! Committed, because a change to any of them changes what the guest can do:
//! every path, its node kind, its permission bits (including setuid, setgid and
//! sticky), its owning uid and gid, the content of every regular file, the
//! target of every symlink, and the device numbers of every device node.
//!
//! Not committed, because none of them is a property of the filesystem's
//! contents: timestamps, inode numbers, link counts, block addresses, and the
//! order the tree happened to be walked in.
//!
//! # Hard links, and what v1 does not cover
//!
//! **Hard links** are committed as what they look like: two paths whose
//! contents are equal. The link count is not committed, so a tree where
//! `/bin/sh` and `/bin/dash` share an inode digests the same as one where they
//! are two copies of the same bytes. That is deliberate — the guest reads the
//! same bytes at the same two paths either way, and the sharing is an
//! allocation detail of the same kind as a block address. It is NOT free: a
//! capsule that depended on a write through one path being visible at the other
//! would be two different guests with one digest. The Step-4 subset ships
//! read-only roots, so no such capsule can exist yet; a lane that allows a
//! writable root must revisit this before it does.
//!
//! **Not covered by v1, and refused rather than ignored where it can be**:
//!
//! * **Extended attributes and POSIX ACLs.** Not read, so two trees differing
//!   only in an xattr or an ACL digest the same. They are outside the Step-4
//!   subset (nothing in it sets one), and reading them would fix their
//!   canonical encoding into the identity forever on the strength of a guess.
//!   A lane that admits them must extend the domain string below, which is what
//!   makes the extension visible rather than silent.
//! * **Sparse files.** A hole and a run of zero bytes digest the same, because
//!   the digest is over content and both read as zeros. The guest cannot tell
//!   them apart either; only the space they occupy differs, which is an
//!   allocation property.
//! * **Anything a `docker export` tarball cannot carry** — mount points,
//!   open file descriptions, and the like — is not in the tree to begin with.

use std::collections::BTreeMap;
use std::path::Path;

use capsule::execution_contract::{ContentDigest, DigestAlgorithm};

/// The domain this digest is taken under. Changing the algorithm below means
/// changing this, so an old digest can never be mistaken for a new one.
pub const GUEST_FILESYSTEM_VIEW_DOMAIN: &str = "ato.guest-filesystem-view/v1";

/// One byte naming what a node IS. A file whose path later becomes a directory
/// is a different filesystem even if nothing else moved.
const KIND_FILE: u8 = b'f';
const KIND_DIR: u8 = b'd';
const KIND_SYMLINK: u8 = b'l';
const KIND_CHAR_DEVICE: u8 = b'c';
const KIND_BLOCK_DEVICE: u8 = b'b';
const KIND_FIFO: u8 = b'p';
const KIND_SOCKET: u8 = b's';

/// Digest the contents of a guest root filesystem tree at `root`.
///
/// Walks every entry, sorts by path bytes, and feeds a domain-separated blake3.
/// Deterministic across hosts and runs: nothing that varies with WHEN or WHERE
/// the tree was produced reaches the hash.
///
/// Fails closed on a node kind it cannot describe rather than skipping it —
/// something in the guest that the digest does not cover is exactly the gap an
/// identity must not have.
pub fn guest_filesystem_digest(root: &Path) -> Result<ContentDigest, String> {
    let mut entries: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
    collect(root, Vec::new(), &mut entries)?;

    let mut hasher = blake3::Hasher::new();
    hasher.update(GUEST_FILESYSTEM_VIEW_DOMAIN.as_bytes());
    hasher.update(&[0]);
    // A BTreeMap keyed by the raw path bytes: the iteration order is the sort
    // order, so it cannot depend on how the directory happened to be read.
    for (path, entry) in &entries {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(&[entry.kind]);
        hasher.update(&entry.mode.to_le_bytes());
        hasher.update(&entry.uid.to_le_bytes());
        hasher.update(&entry.gid.to_le_bytes());
        match &entry.body {
            Body::None => hasher.update(&[0u8]),
            Body::Content { size, digest } => {
                hasher.update(&[1u8]);
                hasher.update(&size.to_le_bytes());
                hasher.update(digest)
            }
            Body::Target(target) => {
                hasher.update(&[2u8]);
                hasher.update(&(target.len() as u64).to_le_bytes());
                hasher.update(target)
            }
            Body::Device { major, minor } => {
                hasher.update(&[3u8]);
                hasher.update(&major.to_le_bytes());
                hasher.update(&minor.to_le_bytes())
            }
        };
    }
    Ok(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    ))
}

struct Entry {
    kind: u8,
    /// Permission bits only (`mode & 0o7777`) — setuid, setgid and sticky
    /// included, because each of them changes what the guest may do. The node
    /// kind is carried separately rather than through the format bits.
    mode: u32,
    uid: u32,
    gid: u32,
    body: Body,
}

enum Body {
    None,
    Content { size: u64, digest: [u8; 32] },
    Target(Vec<u8>),
    Device { major: u32, minor: u32 },
}

fn collect(
    directory: &Path,
    prefix: Vec<u8>,
    into: &mut BTreeMap<Vec<u8>, Entry>,
) -> Result<(), String> {
    let readable = std::fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    for entry in readable {
        let entry = entry.map_err(|error| format!("read {}: {error}", directory.display()))?;
        let path = entry.path();

        let mut relative = prefix.clone();
        relative.push(b'/');
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            relative.extend_from_slice(entry.file_name().as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        {
            relative.extend_from_slice(entry.file_name().to_string_lossy().as_bytes());
        }

        let (record, recurse) = describe(&path)?;
        into.insert(relative.clone(), record);
        if recurse {
            collect(&path, relative, into)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn describe(path: &Path) -> Result<(Entry, bool), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    let file_type = metadata.file_type();
    let common = |kind: u8, body: Body| Entry {
        kind,
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
        body,
    };

    if file_type.is_dir() {
        return Ok((common(KIND_DIR, Body::None), true));
    }
    if file_type.is_symlink() {
        use std::os::unix::ffi::OsStrExt;
        let target = std::fs::read_link(path)
            .map_err(|error| format!("read symlink {}: {error}", path.display()))?;
        return Ok((
            common(
                KIND_SYMLINK,
                Body::Target(target.as_os_str().as_bytes().to_vec()),
            ),
            false,
        ));
    }
    if file_type.is_file() {
        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut file, &mut hasher)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        return Ok((
            common(
                KIND_FILE,
                Body::Content {
                    size: metadata.len(),
                    digest: *hasher.finalize().as_bytes(),
                },
            ),
            false,
        ));
    }

    // A device's identity is its numbers, not its (empty) contents. `rdev` is
    // decomposed so the digest does not depend on the platform's dev_t packing.
    let rdev = metadata.rdev();
    let device = Body::Device {
        major: ((rdev >> 8) & 0xfff) as u32,
        minor: ((rdev & 0xff) | ((rdev >> 12) & 0xfff00)) as u32,
    };
    if file_type.is_char_device() {
        return Ok((common(KIND_CHAR_DEVICE, device), false));
    }
    if file_type.is_block_device() {
        return Ok((common(KIND_BLOCK_DEVICE, device), false));
    }
    if file_type.is_fifo() {
        return Ok((common(KIND_FIFO, Body::None), false));
    }
    if file_type.is_socket() {
        return Ok((common(KIND_SOCKET, Body::None), false));
    }
    Err(format!(
        "{} is a node kind this digest cannot describe; refusing rather than \
         leaving part of the guest uncommitted",
        path.display()
    ))
}

#[cfg(not(unix))]
fn describe(path: &Path) -> Result<(Entry, bool), String> {
    Err(format!(
        "a guest filesystem can only be digested on unix (at {})",
        path.display()
    ))
}

#[cfg(test)]
mod tests;
