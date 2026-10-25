//! Local directory capture uses the same credential exclusions as Workspace.
//! Already-pinned archive transport remains immutable input, not a recapture.

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ato_formation::source::{DownloadedArchive, FileVerifiedArchive, SourceLimits};
use ato_formation_worker::job::digest;
use ato_formation_worker::local::{snapshot_directory, snapshot_directory_file};

const CANARY: &str = "public-local-source-credential-exclusion-canary";

fn scratch() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("capture-policy-")
        .tempdir_in(root)
        .unwrap()
}

fn write(root: &Path, relative: &str, bytes: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn snapshots(root: &Path, work: &Path) -> [Vec<u8>; 2] {
    let memory = snapshot_directory(root).unwrap();
    let mut file = snapshot_directory_file(root, work, 1024 * 1024).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(memory, bytes, "both capture callers must apply one policy");
    [memory, bytes]
}

fn paths(bytes: &[u8]) -> Vec<PathBuf> {
    tar::Archive::new(Cursor::new(bytes))
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().into_owned())
        .collect()
}

#[test]
fn credentials_are_excluded_before_either_archive_is_measured_or_uploaded() {
    let at = scratch();
    let source = at.path().join("source");
    write(&source, "README", "public\n");
    write(&source, "app.py", "print('hello')\n");
    for name in [
        ".env",
        "nested/.env.local",
        "nested/credentials.json",
        ".ato-staging/runner/credentials.json",
        ".ssh/id_ed25519",
        ".aws/config",
        ".gnupg/private-keys-v1.d/private",
        ".capsule/objects/private",
        ".git/config",
        "tls/server.pem",
        "nested/client.key",
    ] {
        write(&source, name, CANARY);
    }
    for (index, bytes) in snapshots(&source, &at.path().join("work"))
        .into_iter()
        .enumerate()
    {
        assert!(
            !bytes
                .windows(CANARY.len())
                .any(|part| part == CANARY.as_bytes())
        );
        let entries = paths(&bytes);
        assert!(entries.contains(&PathBuf::from("source/README")));
        assert!(entries.contains(&PathBuf::from("source/app.py")));
        assert!(!entries.iter().any(|path| {
            let text = path.to_string_lossy();
            text.contains("credentials")
                || text.contains(".env")
                || text.contains(".ssh")
                || text.contains(".aws")
                || text.contains(".gnupg")
                || text.contains(".capsule")
                || text.contains(".git")
                || text.ends_with(".pem")
                || text.ends_with(".key")
        }));
        let verified = DownloadedArchive::new(bytes.clone())
            .verify_archive_digest(&digest(&bytes))
            .unwrap()
            .verify_tree_digest(None, SourceLimits::default())
            .unwrap();
        let destination = at.path().join(format!("materialized-{index}"));
        let materialized = verified
            .materialize(&destination, "", SourceLimits::default())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(materialized.join("README")).unwrap(),
            "public\n"
        );
        assert!(!materialized.join(".env").exists());
        assert!(
            !materialized
                .join(".ato-staging/runner/credentials.json")
                .exists()
        );
        let artifact = ato_formation_worker::pack::pack_tree(&materialized).unwrap();
        assert!(
            !artifact
                .windows(CANARY.len())
                .any(|part| part == CANARY.as_bytes())
        );
    }
}

#[test]
fn changing_an_excluded_secret_does_not_change_the_captured_initial_condition() {
    let at = scratch();
    let source = at.path().join("source");
    write(&source, "app.py", "print('hello')\n");
    write(&source, ".env", "first-public-canary");
    let first = snapshot_directory(&source).unwrap();
    write(&source, ".env", "second-public-canary");
    write(&source, ".capsule/objects/private", CANARY);
    write(&source, ".ssh/id_ed25519", CANARY);
    let second = snapshot_directory(&source).unwrap();
    assert_eq!(digest(&first), digest(&second));
}

#[test]
fn a_clean_local_tree_keeps_its_previous_exact_transport_bytes() {
    let at = scratch();
    let source = at.path().join("source");
    write(&source, "README", "public\n");
    write(&source, "app.py", "print('hello')\n");
    std::fs::create_dir(source.join("empty")).unwrap();
    for bytes in snapshots(&source, &at.path().join("work")) {
        // Captured from the unchanged parent: sorted GNU tar headers, modes,
        // directory entries and source/ wrapper are transport compatibility.
        assert_eq!(
            digest(&bytes),
            "sha256:59a0fb52dc414530475a6019e90c06d743480746d2349802b51fe94bad92a067"
        );
    }
}

#[test]
fn pinned_raw_archives_are_verified_without_applying_local_capture_filters() {
    let at = scratch();
    let mut bytes = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut bytes);
        for (path, value) in [
            ("repo-sha/app.py", "print('hello')\n"),
            ("repo-sha/.env", CANARY),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(value.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            archive
                .append_data(&mut header, path, value.as_bytes())
                .unwrap();
        }
        archive.finish().unwrap();
    }
    let before = digest(&bytes);
    let mut file = tempfile::tempfile_in(at.path()).unwrap();
    std::io::Write::write_all(&mut file, &bytes).unwrap();
    let mut verified =
        FileVerifiedArchive::verify(file, &before, bytes.len() as u64, SourceLimits::default())
            .unwrap();
    assert_eq!(verified.expanded_file_count(), 2);
    assert_eq!(
        verified.expanded_bytes(),
        (CANARY.len() + "print('hello')\n".len()) as u64
    );
    let materialized = verified
        .materialize(&at.path().join("raw-input"), "", SourceLimits::default())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(materialized.join(".env")).unwrap(),
        CANARY
    );
    assert_eq!(digest(&bytes), before);
}

#[cfg(unix)]
#[test]
fn excluded_links_are_skipped_without_reading_their_targets_and_public_links_stay_v2() {
    use ato_formation::source::RESOLVER_CONTRACT_V2;
    use std::os::unix::fs::symlink;
    let at = scratch();
    let source = at.path().join("source");
    write(&source, "app.py", "print('hello')\n");
    write(at.path(), "outside-credentials", CANARY);
    symlink(at.path().join("outside-credentials"), source.join(".env")).unwrap();
    symlink("app.py", source.join("public-link")).unwrap();
    let bytes = snapshot_directory(&source).unwrap();
    assert!(
        !bytes
            .windows(CANARY.len())
            .any(|part| part == CANARY.as_bytes())
    );
    assert!(!paths(&bytes).contains(&PathBuf::from("source/.env")));
    let verified = DownloadedArchive::new(bytes.clone())
        .verify_archive_digest(&digest(&bytes))
        .unwrap()
        .verify_tree_digest(None, SourceLimits::default())
        .unwrap();
    assert_eq!(verified.resolver_contract(), RESOLVER_CONTRACT_V2);
}
