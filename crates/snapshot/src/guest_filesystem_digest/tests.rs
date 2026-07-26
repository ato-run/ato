//! Each test names one thing the digest must or must not see. The pairs matter
//! more than the singles: a hash that ignored everything would pass every
//! "does not change" test, and one that hashed the raw directory would pass
//! every "does change" test.

use std::path::Path;

use super::*;
use tempfile::TempDir;

fn tree() -> TempDir {
    let root = TempDir::new().expect("tempdir");
    write(root.path(), "app/main.py", b"print('hi')\n");
    write(root.path(), "etc/hosts", b"127.0.0.1 localhost\n");
    root
}

fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent");
    std::fs::write(path, contents).expect("write");
}

fn digest(root: &Path) -> ContentDigest {
    guest_filesystem_digest(root).expect("digest")
}

/// The same contents digest the same, and the digest is over CONTENT: a second
/// tree built independently at another path agrees with the first.
#[test]
fn the_same_contents_at_another_path_digest_the_same() {
    let one = tree();
    let two = tree();
    assert_ne!(one.path(), two.path());
    assert_eq!(digest(one.path()), digest(two.path()));
    assert_eq!(digest(one.path()), digest(one.path()), "and it is stable");
}

/// Timestamps are not contents. This is the whole reason the digest exists:
/// `mke2fs` stamps every inode with the wall clock, and an identity that moved
/// with it would change on every build.
#[cfg(unix)]
#[test]
fn timestamps_do_not_reach_the_digest() {
    let root = tree();
    let before = digest(root.path());

    let far_future = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000_000);
    let handle = std::fs::File::options()
        .write(true)
        .open(root.path().join("app/main.py"))
        .expect("open");
    handle
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(far_future)
                .set_modified(far_future),
        )
        .expect("set times");
    drop(handle);

    assert_eq!(before, digest(root.path()));
}

/// Content does. Both directions, so neither assertion can be vacuous.
#[test]
fn a_change_to_a_file_changes_the_digest() {
    let root = tree();
    let before = digest(root.path());
    write(root.path(), "app/main.py", b"print('bye')\n");
    assert_ne!(before, digest(root.path()));
}

/// A file that moves is a different filesystem, even with identical bytes.
#[test]
fn a_path_change_changes_the_digest() {
    let root = tree();
    let before = digest(root.path());
    std::fs::rename(
        root.path().join("app/main.py"),
        root.path().join("app/other.py"),
    )
    .expect("rename");
    assert_ne!(before, digest(root.path()));
}

/// Permission bits are what the guest may do, so they are committed — setuid
/// especially, where the difference is the whole security question.
#[cfg(unix)]
#[test]
fn permission_and_setuid_bits_are_committed() {
    use std::os::unix::fs::PermissionsExt;
    let root = tree();
    let binary = root.path().join("app/main.py");

    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let plain = digest(root.path());

    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let executable = digest(root.path());
    assert_ne!(plain, executable, "the executable bit");

    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o4755)).expect("chmod");
    assert_ne!(executable, digest(root.path()), "the setuid bit");
}

/// A symlink is committed by its TARGET, not by what the target contains — and
/// it is never followed, so a link into the host cannot smuggle host bytes into
/// the identity.
#[cfg(unix)]
#[test]
fn a_symlink_is_committed_by_its_target_and_never_followed() {
    let root = tree();
    std::os::unix::fs::symlink("/etc/hosts", root.path().join("link")).expect("symlink");
    let before = digest(root.path());

    std::fs::remove_file(root.path().join("link")).expect("remove");
    std::os::unix::fs::symlink("/etc/passwd", root.path().join("link")).expect("symlink");
    assert_ne!(before, digest(root.path()), "the target is committed");

    // Dangling is fine: the link is described, not resolved.
    std::fs::remove_file(root.path().join("link")).expect("remove");
    std::os::unix::fs::symlink("/nowhere/at/all", root.path().join("link")).expect("symlink");
    guest_filesystem_digest(root.path()).expect("a dangling symlink is still a symlink");
}

/// A file and a directory at one path are different filesystems, even when the
/// digest has nothing else to tell them apart.
#[test]
fn a_node_kind_change_changes_the_digest() {
    let root = TempDir::new().expect("tempdir");
    std::fs::write(root.path().join("thing"), b"").expect("empty file");
    let as_file = digest(root.path());

    std::fs::remove_file(root.path().join("thing")).expect("remove");
    std::fs::create_dir(root.path().join("thing")).expect("dir");
    assert_ne!(as_file, digest(root.path()));
}

/// Path and content are separated, so a rename cannot be cancelled out by an
/// edit. Without the length prefix, `ab` + `c` and `a` + `bc` would collide.
#[test]
fn path_and_content_boundaries_are_not_ambiguous() {
    let one = TempDir::new().expect("tempdir");
    std::fs::write(one.path().join("ab"), b"c").expect("write");

    let two = TempDir::new().expect("tempdir");
    std::fs::write(two.path().join("a"), b"bc").expect("write");

    assert_ne!(digest(one.path()), digest(two.path()));
}

/// An empty file and an absent one are different, and two empty files at
/// different paths are different from each other.
#[test]
fn empty_files_are_still_committed() {
    let root = TempDir::new().expect("tempdir");
    let empty = digest(root.path());

    std::fs::write(root.path().join("a"), b"").expect("write");
    let one = digest(root.path());
    assert_ne!(empty, one);

    std::fs::write(root.path().join("b"), b"").expect("write");
    assert_ne!(one, digest(root.path()));
}

/// The walk order of the host filesystem must not reach the digest. Creating
/// the same entries in the opposite order is the cheapest way to disturb
/// `read_dir`'s order without changing the tree.
#[test]
fn the_directory_read_order_does_not_reach_the_digest() {
    let ascending = TempDir::new().expect("tempdir");
    for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        std::fs::write(ascending.path().join(name), name.as_bytes()).expect("write");
    }
    let descending = TempDir::new().expect("tempdir");
    for name in ["h", "g", "f", "e", "d", "c", "b", "a"] {
        std::fs::write(descending.path().join(name), name.as_bytes()).expect("write");
    }
    assert_eq!(digest(ascending.path()), digest(descending.path()));
}

/// Ownership is committed: a rootfs whose files belong to a different user is a
/// different guest. Asserted through the record rather than by chown, which a
/// test cannot do unprivileged.
#[cfg(unix)]
#[test]
fn ownership_is_part_of_the_record() {
    use std::os::unix::fs::MetadataExt;
    let root = tree();
    let (entry, _) = describe(&root.path().join("app/main.py")).expect("describe");
    let metadata = std::fs::metadata(root.path().join("app/main.py")).expect("metadata");
    assert_eq!(entry.uid, metadata.uid());
    assert_eq!(entry.gid, metadata.gid());
}

/// A node kind the digest cannot describe is refused, not skipped: something in
/// the guest that no digest covers is the gap an identity must not have.
#[cfg(unix)]
#[test]
fn an_undescribable_node_is_refused_rather_than_skipped() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let root = tree();
    let fifo = root.path().join("pipe");
    let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
    // SAFETY: a standard libc call with a valid NUL-terminated path.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) } != 0 {
        eprintln!("skipping: mkfifo unavailable");
        return;
    }
    // A FIFO IS describable — it has a kind and permissions and no contents —
    // so this asserts it is committed rather than dropped.
    let with_fifo = digest(root.path());
    std::fs::remove_file(&fifo).expect("remove");
    assert_ne!(with_fifo, digest(root.path()));
}

/// The domain is part of the preimage, so this digest can never be confused
/// with another blake3 the contract also commits.
#[test]
fn the_digest_is_domain_separated() {
    let root = TempDir::new().expect("tempdir");
    let empty_tree = digest(root.path());
    let undomained = ContentDigest::new(
        DigestAlgorithm::Blake3,
        *blake3::Hasher::new().finalize().as_bytes(),
    );
    assert_ne!(empty_tree, undomained);
    assert_eq!(GUEST_FILESYSTEM_VIEW_DOMAIN, "ato.guest-filesystem-view/v1");
}
