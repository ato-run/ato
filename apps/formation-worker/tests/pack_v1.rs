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

#[cfg(unix)]
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

#[cfg(unix)]
fn packed_entries(bytes: &[u8]) -> Vec<(String, tar::EntryType, Option<String>, u32, u64)> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let header = entry.header();
            (
                entry.path().unwrap().display().to_string(),
                header.entry_type(),
                entry
                    .link_name()
                    .unwrap()
                    .map(|target| target.display().to_string()),
                header.mode().unwrap(),
                header.mtime().unwrap(),
            )
        })
        .collect()
}

/// `real/file.txt` and `alias -> real`.
#[cfg(unix)]
fn aliased(target: &str) -> tempfile::TempDir {
    let dir = tree(&[("real/file.txt", "hello")]);
    std::os::unix::fs::symlink(target, dir.path().join("alias")).expect("symlink");
    dir
}

#[cfg(unix)]
#[test]
fn a_contained_symlink_is_packed_as_a_link() {
    let dir = aliased("real");
    let packed = pack_tree(dir.path()).expect("packs");
    let entries = packed_entries(&packed);
    let alias = entries
        .iter()
        .find(|(path, ..)| path == "alias")
        .expect("the link is an entry");
    assert_eq!(alias.1, tar::EntryType::Symlink);
    assert_eq!(alias.2.as_deref(), Some("real"));
    // Normalized like every other entry.
    assert_eq!((alias.3, alias.4), (0o777, 0));
    // Never flattened: no second copy of the target's content.
    assert_eq!(
        entries
            .iter()
            .filter(|(path, ..)| path.ends_with("file.txt"))
            .count(),
        1
    );

    // Expanded, it reads through as it did in the built tree.
    let out = tempfile::tempdir().unwrap();
    tar::Archive::new(std::io::Cursor::new(&packed))
        .unpack(out.path())
        .expect("unpacks");
    assert!(
        std::fs::symlink_metadata(out.path().join("alias"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(out.path().join("alias/file.txt")).unwrap(),
        "hello"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_tree_packs_deterministically_and_the_target_is_its_identity() {
    let one = pack_tree(aliased("real").path()).unwrap();
    let two = pack_tree(aliased("real").path()).unwrap();
    assert_eq!(digest(&one), digest(&two));
    let other = {
        let dir = aliased("real");
        std::fs::create_dir(dir.path().join("elsewhere")).unwrap();
        std::fs::write(dir.path().join("elsewhere/file.txt"), "hello").unwrap();
        std::fs::remove_file(dir.path().join("alias")).unwrap();
        std::os::unix::fs::symlink("elsewhere", dir.path().join("alias")).unwrap();
        let with_other = pack_tree(dir.path()).unwrap();
        // Same files, same link path — only the target differs.
        std::fs::remove_file(dir.path().join("alias")).unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
        (with_other, pack_tree(dir.path()).unwrap())
    };
    assert_ne!(digest(&other.0), digest(&other.1));
}

#[cfg(unix)]
#[test]
fn a_symlink_that_leaves_the_tree_is_refused_by_its_tree_path() {
    for (link, target) in [
        ("alias", "/etc/passwd"),
        (
            "venv/bin/python3",
            "/opt/ato/toolchains/python/3.12.7/bin/python3",
        ),
        ("alias", "../../outside"),
        ("deep/alias", "../../../outside"),
        ("alias", "real/../../outside"),
    ] {
        let dir = tree(&[("real/file.txt", "hello")]);
        let at = dir.path().join(link);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, &at).expect("symlink");
        let error = format!("{:#}", pack_tree(dir.path()).expect_err(link));
        assert!(error.contains(link), "{error}");
        // The worker's scratch path is not part of what a requester sees.
        assert!(
            !error.contains(&*dir.path().to_string_lossy()),
            "host path in {error}"
        );
    }
}

/// A fixed symlink-free tree: files, an executable, nested and empty
/// directories.
#[cfg(unix)]
fn golden_tree() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("app/static")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("README"), "readme\n").unwrap();
    std::fs::write(root.join("app/main.py"), "print('hi')\n").unwrap();
    std::fs::write(root.join("app/static/site.css"), "body{}\n").unwrap();
    std::fs::write(root.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();
    for (path, mode) in [
        ("README", 0o644),
        ("app/main.py", 0o644),
        ("app/static/site.css", 0o600),
        ("run.sh", 0o755),
    ] {
        std::fs::set_permissions(root.join(path), std::fs::Permissions::from_mode(mode)).unwrap();
    }
    dir
}

/// Recorded from `pack_tree` before artifacts could carry symlinks: a
/// symlink-free tree must keep this exact address.
#[cfg(unix)]
const GOLDEN_SYMLINK_FREE_ARTIFACT: &str =
    "sha256:eed3d385e00bc60ce83670334034a49839f669245b1a8067eca5b435f4e89de5";

#[cfg(unix)]
#[test]
fn a_symlink_free_artifact_keeps_its_exact_bytes() {
    let tree = golden_tree();
    let packed = pack_tree(tree.path()).expect("packs");
    assert_eq!(digest(&packed), GOLDEN_SYMLINK_FREE_ARTIFACT);
}
