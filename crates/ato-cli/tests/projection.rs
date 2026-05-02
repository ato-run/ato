//! Tests for `application::projection`. Asserts that projecting a frozen
//! blob into a session workspace recreates the tree faithfully and that
//! mutating the projection never corrupts the immutable source.

use std::fs;
use std::path::Path;

use ato_cli::projection::{project_payload, ProjectionError, ProjectionStrategy};
use tempfile::TempDir;

fn write_file(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn projection_recreates_files_and_sizes() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("session/deps");
    write_file(
        &payload,
        "node_modules/foo/index.js",
        b"console.log('foo');\n",
    );
    write_file(
        &payload,
        "node_modules/bar/lib.js",
        b"module.exports = {};\n",
    );

    let outcome = project_payload(&payload, &target).unwrap();
    assert!(outcome.file_count >= 2);
    assert!(target.join("node_modules/foo/index.js").is_file());
    assert!(target.join("node_modules/bar/lib.js").is_file());

    let original = fs::read(payload.join("node_modules/foo/index.js")).unwrap();
    let projected = fs::read(target.join("node_modules/foo/index.js")).unwrap();
    assert_eq!(original, projected);
}

#[test]
fn projection_refuses_existing_target() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("deps");
    write_file(&payload, "x.txt", b"y");
    fs::create_dir_all(&target).unwrap();

    let err = project_payload(&payload, &target).unwrap_err();
    let downcast = err.downcast_ref::<ProjectionError>();
    assert!(matches!(downcast, Some(ProjectionError::TargetExists(_))));
}

#[test]
fn projection_errors_when_payload_is_missing() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("nope");
    let target = tmp.path().join("deps");
    let err = project_payload(&payload, &target).unwrap_err();
    let downcast = err.downcast_ref::<ProjectionError>();
    assert!(matches!(
        downcast,
        Some(ProjectionError::PayloadNotDirectory(_))
    ));
}

#[cfg(unix)]
#[test]
fn projection_recreates_symlinks_without_following() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("deps");
    write_file(&payload, "real.txt", b"hello");
    symlink("real.txt", payload.join("link")).unwrap();

    let outcome = project_payload(&payload, &target).unwrap();
    assert!(outcome.symlink_count >= 1);

    let projected_link = target.join("link");
    let metadata = fs::symlink_metadata(&projected_link).unwrap();
    assert!(metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_link(&projected_link).unwrap(),
        Path::new("real.txt")
    );
}

#[cfg(unix)]
#[test]
fn modifying_projection_does_not_corrupt_payload() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("deps");
    write_file(&payload, "data.txt", b"original");

    let outcome = project_payload(&payload, &target).unwrap();
    assert!(matches!(
        outcome.strategy,
        ProjectionStrategy::Clonefile | ProjectionStrategy::Copy
    ));

    // Make the projection writable in case the platform projection mode
    // dropped the user-write bit, then mutate the projection.
    let projected = target.join("data.txt");
    let mut perms = fs::metadata(&projected).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o600);
    }
    fs::set_permissions(&projected, perms).unwrap();
    fs::write(&projected, b"mutated").unwrap();

    let original = fs::read(payload.join("data.txt")).unwrap();
    assert_eq!(
        original, b"original",
        "modifying the projection must not write through to the payload"
    );
    let projected_after = fs::read(&projected).unwrap();
    assert_eq!(projected_after, b"mutated");
}

#[test]
fn projection_returns_clonefile_or_copy_strategy() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("deps");
    write_file(&payload, "a.txt", b"a");
    let outcome = project_payload(&payload, &target).unwrap();
    assert!(matches!(
        outcome.strategy,
        ProjectionStrategy::Clonefile | ProjectionStrategy::Copy
    ));
}

#[test]
fn projection_recreates_empty_directories() {
    let tmp = TempDir::new().unwrap();
    let payload = tmp.path().join("payload");
    let target = tmp.path().join("deps");
    write_file(&payload, "a.txt", b"a");
    fs::create_dir_all(payload.join("scaffold/empty")).unwrap();

    project_payload(&payload, &target).unwrap();
    assert!(target.join("scaffold/empty").is_dir());
}
