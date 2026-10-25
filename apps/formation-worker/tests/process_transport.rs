use ato_formation::source::{DownloadedArchive, FileVerifiedArchive, SourceLimits};
use ato_formation_worker::pack::{pack_process_artifact, pack_tree};
use sha2::{Digest, Sha256};

fn reference(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[test]
fn small_process_transport_keeps_existing_bytes() {
    std::fs::create_dir_all(".tmp").unwrap();
    let tree = tempfile::tempdir_in(".tmp").unwrap();
    std::fs::write(tree.path().join("app.py"), b"print('application')\n").unwrap();
    assert_eq!(
        pack_process_artifact(tree.path()).unwrap(),
        pack_tree(tree.path()).unwrap()
    );
}

#[test]
fn compressed_process_transport_preserves_tree_and_expansion_bounds() {
    std::fs::create_dir_all(".tmp").unwrap();
    let tree = tempfile::tempdir_in(".tmp").unwrap();
    let data = vec![b'a'; 8 * 1024 * 1024];
    std::fs::write(tree.path().join("data.bin"), &data).unwrap();
    std::fs::create_dir(tree.path().join("empty")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("data.bin", tree.path().join("alias")).unwrap();
    let raw = pack_tree(tree.path()).unwrap();
    let packed = pack_process_artifact(tree.path()).unwrap();
    assert!(packed.len() < raw.len() / 10);
    assert_eq!(packed, pack_process_artifact(tree.path()).unwrap());
    let limits = SourceLimits::default();
    let verify = |bytes: Vec<u8>| {
        let digest = reference(&bytes);
        DownloadedArchive::new(bytes)
            .verify_archive_digest(&digest)
            .unwrap()
            .verify_tree_digest(None, limits)
            .unwrap()
    };
    let raw_tree = verify(raw);
    let compressed_tree = verify(packed.clone());
    assert_eq!(
        raw_tree.closure_ref("").unwrap(),
        compressed_tree.closure_ref("").unwrap()
    );
    assert_eq!(compressed_tree.expanded_bytes(), data.len() as u64);
    let mut file = tempfile::tempfile_in(".tmp").unwrap();
    std::io::Write::write_all(&mut file, &packed).unwrap();
    use std::io::Seek;
    file.rewind().unwrap();
    let mut archive =
        FileVerifiedArchive::verify(file, &reference(&packed), packed.len() as u64, limits)
            .unwrap();
    let output = tempfile::tempdir_in(".tmp").unwrap();
    let materialized = archive.materialize(output.path(), "", limits).unwrap();
    assert_eq!(std::fs::read(materialized.join("data.bin")).unwrap(), data);
    assert!(materialized.join("empty").is_dir());
    let bounded = SourceLimits {
        max_total_bytes: 1024 * 1024,
        ..limits
    };
    let rejected = DownloadedArchive::new(packed.clone())
        .verify_archive_digest(&reference(&packed))
        .unwrap()
        .verify_tree_digest(None, bounded);
    assert!(
        rejected.is_err(),
        "compression must never bypass the expanded-byte cap"
    );
}
