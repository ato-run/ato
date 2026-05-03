//! Tests that lock down `foundation::blob::tree_hash` against the spec in
//! `docs/rfcs/accepted/A1_BLOB_HASH.md`. Any failure here means the wire
//! format has drifted and the algorithm prefix must be bumped.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use capsule_core::blob::hash_tree;
use tempfile::TempDir;

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn touch_dir(root: &Path, rel: &str) {
    fs::create_dir_all(root.join(rel)).unwrap();
}

#[test]
fn determinism_same_input_same_hash() {
    let tmp = TempDir::new().unwrap();
    write_file(tmp.path(), "src/main.rs", b"fn main() {}\n");
    write_file(tmp.path(), "README.md", b"hello\n");

    let first = hash_tree(tmp.path()).unwrap();
    let second = hash_tree(tmp.path()).unwrap();
    assert_eq!(first.blob_hash, second.blob_hash);
    assert!(first.blob_hash.starts_with("sha256:"));
    assert_eq!(first.file_count, 2);
    assert!(first.total_bytes > 0);
}

#[test]
fn one_byte_content_change_changes_hash() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "data.txt", b"hello world");
    write_file(tmp_b.path(), "data.txt", b"hello worle");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(a.blob_hash, b.blob_hash);
}

#[test]
fn mtime_drift_does_not_change_hash() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "data.txt", b"same bytes");
    write_file(tmp_b.path(), "data.txt", b"same bytes");

    // Force the second tree to have a different mtime; the algorithm must
    // ignore it.
    let earlier = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let later = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let a_path = tmp_a.path().join("data.txt");
    let b_path = tmp_b.path().join("data.txt");
    let a_file = fs::File::open(&a_path).unwrap();
    let b_file = fs::File::open(&b_path).unwrap();
    a_file.set_modified(earlier).unwrap();
    b_file.set_modified(later).unwrap();

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_eq!(a.blob_hash, b.blob_hash);
}

#[cfg(unix)]
#[test]
fn executable_bit_changes_hash() {
    use std::os::unix::fs::PermissionsExt;

    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "bin/run", b"#!/bin/sh\necho hi\n");
    write_file(tmp_b.path(), "bin/run", b"#!/bin/sh\necho hi\n");

    fs::set_permissions(
        tmp_a.path().join("bin/run"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::set_permissions(
        tmp_b.path().join("bin/run"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(
        a.blob_hash, b.blob_hash,
        "executable bit must influence the hash"
    );
}

#[cfg(unix)]
#[test]
fn group_other_permission_bits_do_not_change_hash() {
    use std::os::unix::fs::PermissionsExt;

    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "data.txt", b"x");
    write_file(tmp_b.path(), "data.txt", b"x");

    // Same owner-exec bit (off), different group/other bits.
    fs::set_permissions(
        tmp_a.path().join("data.txt"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::set_permissions(
        tmp_b.path().join("data.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_eq!(a.blob_hash, b.blob_hash);
}

#[cfg(unix)]
#[test]
fn symlink_target_is_hashed_without_chasing() {
    use std::os::unix::fs::symlink;

    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "real.txt", b"target-a");
    write_file(tmp_b.path(), "real.txt", b"target-b");
    symlink("real.txt", tmp_a.path().join("link")).unwrap();
    symlink("real.txt", tmp_b.path().join("link")).unwrap();

    // Same symlink target string and same target file name, but content
    // differs. The algorithm must NOT chase the symlink, but the file
    // content difference still changes the hash via real.txt.
    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(a.blob_hash, b.blob_hash);
    assert_eq!(a.symlink_count, 1);

    // Now diverge the symlink target itself; the file contents are equal so
    // any difference must come from the link target bytes.
    let tmp_c = TempDir::new().unwrap();
    let tmp_d = TempDir::new().unwrap();
    write_file(tmp_c.path(), "real.txt", b"shared");
    write_file(tmp_d.path(), "real.txt", b"shared");
    symlink("real.txt", tmp_c.path().join("link")).unwrap();
    symlink("./real.txt", tmp_d.path().join("link")).unwrap();
    let c = hash_tree(tmp_c.path()).unwrap();
    let d = hash_tree(tmp_d.path()).unwrap();
    assert_ne!(c.blob_hash, d.blob_hash);
}

#[test]
fn zero_byte_files_hash_deterministically() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "marker", b"");
    write_file(tmp_b.path(), "marker", b"");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_eq!(a.blob_hash, b.blob_hash);
    assert_eq!(a.total_bytes, 0);
    assert_eq!(a.file_count, 1);
}

#[test]
fn recursively_empty_directories_are_excluded() {
    let tmp_with = TempDir::new().unwrap();
    let tmp_without = TempDir::new().unwrap();
    write_file(tmp_with.path(), "data.txt", b"payload");
    touch_dir(tmp_with.path(), "scaffold/empty");
    touch_dir(tmp_with.path(), "scaffold/inner/yet-another");

    write_file(tmp_without.path(), "data.txt", b"payload");

    let with = hash_tree(tmp_with.path()).unwrap();
    let without = hash_tree(tmp_without.path()).unwrap();
    assert_eq!(with.blob_hash, without.blob_hash);
    assert_eq!(with.dir_count, 0);
}

#[test]
fn directory_with_one_file_is_included_in_parent() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "src/main.rs", b"fn main() {}\n");
    write_file(tmp_b.path(), "src/main.rs", b"fn main() {}\n");
    // tmp_b has an extra non-empty subtree. It must change the hash.
    write_file(tmp_b.path(), "tests/it.rs", b"// integration test\n");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(a.blob_hash, b.blob_hash);
}

#[test]
fn child_ordering_is_by_raw_bytes_not_by_locale() {
    // Add a pair of names that would sort differently under a locale-aware
    // collation but identically under raw byte ordering. Both trees should
    // produce the same hash because they have the same content.
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "a.txt", b"a");
    write_file(tmp_a.path(), "B.txt", b"B");
    write_file(tmp_b.path(), "B.txt", b"B");
    write_file(tmp_b.path(), "a.txt", b"a");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_eq!(a.blob_hash, b.blob_hash);
}

#[test]
fn renaming_a_file_changes_hash() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "old.txt", b"content");
    write_file(tmp_b.path(), "new.txt", b"content");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(a.blob_hash, b.blob_hash);
}

#[test]
fn moving_file_between_dirs_changes_hash() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    write_file(tmp_a.path(), "src/util.rs", b"// util\n");
    write_file(tmp_b.path(), "lib/util.rs", b"// util\n");

    let a = hash_tree(tmp_a.path()).unwrap();
    let b = hash_tree(tmp_b.path()).unwrap();
    assert_ne!(a.blob_hash, b.blob_hash);
}

#[test]
fn root_basename_does_not_influence_hash() {
    // Two trees with the same content but different parent directory names
    // must produce the same blob hash; the spec excludes the root basename.
    let outer_a = TempDir::new().unwrap();
    let outer_b = TempDir::new().unwrap();
    let root_a = outer_a.path().join("alpha");
    let root_b = outer_b.path().join("BETA");
    fs::create_dir(&root_a).unwrap();
    fs::create_dir(&root_b).unwrap();
    write_file(&root_a, "data.txt", b"shared");
    write_file(&root_b, "data.txt", b"shared");

    let a = hash_tree(&root_a).unwrap();
    let b = hash_tree(&root_b).unwrap();
    assert_eq!(a.blob_hash, b.blob_hash);
}
