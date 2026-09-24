//! Contained symlinks in a source tree (resolver v2), end to end: one
//! identity whether the tree arrives as a local directory or an archive, the
//! link preserved as a link from source to staged workspace, usable by a
//! contained build — and no way out of the build sandbox through a link.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use ato_formation::intent::{
    BuildStepV1, EFFECTIVE_BUILD_PLAN_V1_SCHEMA, EffectiveBuildPlanV1, Lane,
};
use ato_formation::source::{
    DownloadedArchive, RESOLVER_CONTRACT_V1, RESOLVER_CONTRACT_V2, SourceLimits,
    TreeVerifiedArchive,
};
use ato_formation_worker::build::{BuildAttempt, run_build};
use ato_formation_worker::job::{copy_tree, digest};
use ato_formation_worker::local::snapshot_directory;
use ato_formation_worker::sandbox::{
    BuildLimits, BuildSandbox, NetworkPolicy, containment_available,
};

fn verify(bytes: Vec<u8>) -> Result<TreeVerifiedArchive, ato_formation::source::SourceError> {
    let archive_digest = digest(&bytes);
    DownloadedArchive::new(bytes)
        .verify_archive_digest(&archive_digest)?
        .verify_tree_digest(None, SourceLimits::default())
}

/// `README`, `safe/file.txt` and `link -> safe/file.txt`, on a disk.
fn linked_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README"), "r").unwrap();
    std::fs::create_dir(dir.path().join("safe")).unwrap();
    std::fs::write(dir.path().join("safe/file.txt"), "hello").unwrap();
    symlink("safe/file.txt", dir.path().join("link")).unwrap();
    dir
}

/// The same tree as a codeload-style archive: wrapped in `<repo>-<sha>/`,
/// other entry order, other mtimes.
fn linked_codeload_archive() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        let mut dir = |path: &str| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o775);
            header.set_mtime(1_700_000_000);
            builder
                .append_data(&mut header, path, std::io::empty())
                .unwrap();
        };
        dir("searx-demo-0123abc/");
        dir("searx-demo-0123abc/safe/");
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        link.set_mtime(1_700_000_001);
        builder
            .append_link(&mut link, "searx-demo-0123abc/link", "safe/file.txt")
            .unwrap();
        for (path, contents) in [
            ("searx-demo-0123abc/safe/file.txt", "hello"),
            ("searx-demo-0123abc/README", "r"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(contents.len() as u64);
            header.set_mode(0o664);
            header.set_mtime(1_700_000_002);
            builder
                .append_data(&mut header, path, Cursor::new(contents.as_bytes()))
                .unwrap();
        }
        builder.finish().unwrap();
    }
    bytes
}

#[test]
fn a_local_directory_and_an_archive_of_the_same_tree_have_one_v2_closure() {
    let dir = linked_dir();
    let local = verify(snapshot_directory(dir.path()).unwrap()).expect("local tree verifies");
    let archive = verify(linked_codeload_archive()).expect("archive verifies");
    assert_eq!(local.resolver_contract(), RESOLVER_CONTRACT_V2);
    assert_eq!(archive.resolver_contract(), RESOLVER_CONTRACT_V2);
    assert_eq!(local.tree_digest(), archive.tree_digest());
    assert_eq!(
        local.closure_ref("").unwrap(),
        archive.closure_ref("").unwrap()
    );
    // The transport bytes differ; the tree does not.
    assert_ne!(
        digest(&snapshot_directory(dir.path()).unwrap()),
        digest(&linked_codeload_archive())
    );
}

#[test]
fn a_tree_without_links_stays_v1() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README"), "r").unwrap();
    let verified = verify(snapshot_directory(dir.path()).unwrap()).unwrap();
    assert_eq!(verified.resolver_contract(), RESOLVER_CONTRACT_V1);
}

#[test]
fn an_escaping_link_in_a_local_directory_is_refused() {
    for (link, target) in [("link", "/etc/passwd"), ("sub/link", "../../outside")] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README"), "r").unwrap();
        let at = dir.path().join(link);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        symlink(target, &at).unwrap();
        let error = verify(snapshot_directory(dir.path()).unwrap())
            .expect_err(&format!("{link} -> {target}"));
        assert_eq!(error.code(), "source_symlink_escape", "{link} -> {target}");
    }
}

fn materialized(scratch: &Path) -> PathBuf {
    verify(snapshot_directory(linked_dir().path()).unwrap())
        .unwrap()
        .materialize(&scratch.join("source"), "", SourceLimits::default())
        .unwrap()
}

#[test]
fn staging_recreates_the_link_as_a_link() {
    let scratch = tempfile::tempdir().unwrap();
    let source = materialized(scratch.path());
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    copy_tree(&source, &workspace).unwrap();
    let link = workspace.join("link");
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        PathBuf::from("safe/file.txt")
    );
}

fn build(workspace: &Path, source: &Path, script: &str) -> anyhow::Result<()> {
    let plan = EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        workspace_guest_root: "/app".to_owned(),
        runtime: BTreeMap::new(),
        steps: vec![BuildStepV1 {
            name: "script".to_owned(),
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            needs_network: false,
            cwd_relative: String::new(),
            env: BTreeMap::new(),
            toolchain_access: ato_formation::intent::ToolchainAccess::ReadOnly,
        }],
        output_root: String::new(),
        toolchain_path: Vec::new(),
    };
    let policy = workspace.parent().unwrap().join("policy.json");
    run_build(
        &plan,
        BuildAttempt {
            job_id: "job".to_owned(),
            attempt_id: "attempt".to_owned(),
            attempt_fence: 1,
        },
        &BuildSandbox {
            source_root: source,
            workspace_root: workspace,
            cache_root: None,
            shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
            policy_host_path: &policy,
            network: NetworkPolicy::Denied,
            limits: BuildLimits::default(),
            toolchain: ato_formation_worker::sandbox::ToolchainAccess::ReadOnly,
        },
    )
    .map(|_| ())
}

#[test]
fn a_contained_build_reads_through_a_contained_link() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let source = materialized(scratch.path());
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    copy_tree(&source, &workspace).unwrap();
    build(
        &workspace,
        &source,
        "cat link > copied.txt && cat /src/link > from-src.txt",
    )
    .expect("the build reads through the link");
    assert_eq!(
        std::fs::read_to_string(workspace.join("copied.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("from-src.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn no_link_reaches_the_host_from_inside_the_build_sandbox() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    // A canary in the real home directory, and links to it placed straight
    // onto the staged workspace — past the resolver, which would refuse them —
    // to measure the sandbox itself.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let secret = format!("ATO_BUILD_HOST_SECRET_{}", std::process::id());
    let canary = home.join(format!(
        "ato-build-symlink-canary-{}.txt",
        std::process::id()
    ));
    std::fs::write(&canary, &secret).unwrap();
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Remove(canary.clone());

    let scratch = tempfile::tempdir().unwrap();
    let source = materialized(scratch.path());
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    copy_tree(&source, &workspace).unwrap();
    symlink(&canary, workspace.join("absolute")).unwrap();
    // `workspace` is `<scratch>/workspace`: climbing far enough from it on
    // the host lands on `/`, and from there on the canary.
    let climbs = workspace.components().count();
    let relative: PathBuf = std::iter::repeat_n("..", climbs)
        .collect::<PathBuf>()
        .join(canary.strip_prefix("/").unwrap());
    symlink(&relative, workspace.join("relative")).unwrap();
    assert_eq!(
        std::fs::read_to_string(workspace.join("relative")).unwrap(),
        secret
    );

    build(
        &workspace,
        &source,
        "for l in absolute relative; do cat $l > out-$l.txt 2>&1 || echo unreachable >> out-$l.txt; done",
    )
    .expect("the build runs");
    for name in ["absolute", "relative"] {
        let out = std::fs::read_to_string(workspace.join(format!("out-{name}.txt"))).unwrap();
        assert!(!out.contains(&secret), "{name}: {out}");
        assert!(out.contains("unreachable"), "{name}: {out}");
    }
}
