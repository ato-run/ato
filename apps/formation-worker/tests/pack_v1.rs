//! Packing a built tree into the artifact the Runner materializes from.
//!
//! The digest IS the artifact's identity, so two builds of the same tree must
//! produce the same address — otherwise nothing downstream can coalesce or
//! compare them.

use ato_formation_worker::pack::pack_tree;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn the_same_tree_packs_to_the_same_address() {
    let files: &[(&str, &str)] = &[("app.py", "print(1)\n"), ("lib/util.py", "x = 1\n")];
    let first = tree(files);
    let second = tree(files);
    // Written at different moments. If mtime reached the archive, every
    // rebuild of unchanged code would mint a new artifact.
    assert_eq!(
        digest(&pack_tree(first.path()).expect("packs")),
        digest(&pack_tree(second.path()).expect("packs"))
    );
}

#[test]
fn different_content_is_a_different_artifact() {
    let one = tree(&[("a.py", "1")]);
    let two = tree(&[("a.py", "2")]);
    let moved = tree(&[("b.py", "1")]);
    let d = |t: &tempfile::TempDir| digest(&pack_tree(t.path()).expect("packs"));
    assert_ne!(d(&one), d(&two), "content must matter");
    assert_ne!(d(&one), d(&moved), "path must matter");
}

#[test]
fn an_executable_keeps_its_bit_and_a_data_file_does_not_gain_one() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tree(&[("bin/run", "#!/bin/sh\n"), ("data.txt", "plain")]);
    std::fs::set_permissions(
        dir.path().join("bin/run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");

    let packed = pack_tree(dir.path()).expect("packs");
    let mut archive = tar::Archive::new(std::io::Cursor::new(packed));
    let mut modes = std::collections::BTreeMap::new();
    for entry in archive.entries().expect("entries") {
        let entry = entry.expect("entry");
        let path = entry.path().expect("path").display().to_string();
        modes.insert(path, entry.header().mode().unwrap_or(0));
    }
    // A workspace whose interpreter arrives non-executable does not start.
    assert_eq!(modes.get("bin/run").copied().unwrap_or(0) & 0o100, 0o100);
    assert_eq!(modes.get("data.txt").copied().unwrap_or(0) & 0o111, 0);
}

#[test]
fn an_empty_directory_is_part_of_the_artifact() {
    let with_dir = tree(&[("a.py", "1")]);
    std::fs::create_dir_all(with_dir.path().join("static")).expect("mkdir");
    let without = tree(&[("a.py", "1")]);
    // A `static/` that vanished between build and run is a different artifact.
    assert_ne!(
        digest(&pack_tree(with_dir.path()).expect("packs")),
        digest(&pack_tree(without.path()).expect("packs"))
    );
}

#[test]
fn a_symlink_fails_the_build_where_it_is() {
    let dir = tree(&[("real.py", "1")]);
    std::os::unix::fs::symlink("real.py", dir.path().join("link.py")).expect("symlink");
    let error = pack_tree(dir.path()).unwrap_err();
    // Following it would silently duplicate the target — a venv's `python3`
    // link becoming a second copy of the interpreter. Refused here so the
    // build fails at the link, not where it is later missed.
    assert!(format!("{error}").contains("symlink"), "{error}");
}
