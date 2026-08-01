//! The v1 producer lane, exercised through its real code with the two
//! host-privileged operations stood in for.
//!
//! [`FakeProducer`] replaces exactly two things: the `docker` invocations and
//! the `mount`-based ext4 packing. It is not a stub of the lane — it records
//! what it was handed and produces an image whose bytes are a function of the
//! projected source it received, so "the guest got the projection" and "a
//! source change moves the identity" are observations about the real lane
//! rather than assertions about the double. Everything above those two calls —
//! the freeze, the projection, the recipe derivation, the observation, the
//! mint, the atomic write, the trusted-load, the comparison — is production
//! code here.
//!
//! The real producer running docker and mkfs is ADR-015 step 6.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use capsule::execution_contract::{
    ContentDigest, ExecutionContractEnvelopeV1, ResolvedTargetContract,
};
use tempfile::TempDir;

use super::*;

const PYTHON_SLIM: &str = "python:3.11-slim";

fn pinned_ref(image: &str, byte: char) -> String {
    format!(
        "docker.io/library/{image}@sha256:{}",
        byte.to_string().repeat(64)
    )
}

fn digest(byte: char) -> ContentDigest {
    ContentDigest::try_from(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("a canonical digest")
}

fn linux_gnu_x86_64() -> ResolvedTargetContract {
    ResolvedTargetContract {
        os: "linux".into(),
        architecture: "x86_64".into(),
        abi: "gnu".into(),
        libc: Some("glibc".into()),
        observable_features: BTreeMap::new(),
    }
}

/// What the lane handed the producer, kept so a test can assert on the guest's
/// actual inputs rather than on the lane's intentions.
#[derive(Debug, Default, Clone)]
struct ProducerLog {
    /// Every path present in the tree `assemble` was pointed at, relative and
    /// sorted — i.e. exactly what the guest's `COPY` would pick up.
    guest_files: Vec<String>,
    pinned_base_refs: Vec<String>,
    resolved_images: Vec<String>,
    measured_images: Vec<String>,
    assembled_argv: Vec<Vec<String>>,
    filesystem_uuids: Vec<String>,
    packed: bool,
    discarded: Vec<String>,
}

/// How the double answers one producer question, keyed by image reference so a
/// test can make the answer depend on which image it was asked about.
type Answer<T> = Box<dyn Fn(&str) -> Result<T, String>>;

struct FakeProducer {
    runtime: Answer<ResolvedRuntimeArtifact>,
    target: Answer<ResolvedTargetContract>,
    log: RefCell<ProducerLog>,
    /// The bytes `pack` will write, derived from the projected tree `assemble`
    /// saw — so a change to the program source is a change to the image, the
    /// way a real build makes it one.
    image_bytes: RefCell<Vec<u8>>,
}

impl FakeProducer {
    /// A producer that resolves any image to a digest-pinned reference and
    /// measures a linux/x86_64/gnu guest.
    fn healthy() -> Self {
        Self {
            runtime: Box::new(|image_ref| {
                Ok(ResolvedRuntimeArtifact {
                    original_ref: image_ref.to_string(),
                    resolved_ref: pinned_ref(image_ref, 'c'),
                    digest: digest('c'),
                })
            }),
            target: Box::new(|_| Ok(linux_gnu_x86_64())),
            log: RefCell::new(ProducerLog::default()),
            image_bytes: RefCell::new(Vec::new()),
        }
    }

    fn measuring(
        target: impl Fn(&str) -> Result<ResolvedTargetContract, String> + 'static,
    ) -> Self {
        Self {
            target: Box::new(target),
            ..Self::healthy()
        }
    }

    fn resolving(
        runtime: impl Fn(&str) -> Result<ResolvedRuntimeArtifact, String> + 'static,
    ) -> Self {
        Self {
            runtime: Box::new(runtime),
            ..Self::healthy()
        }
    }

    fn log(&self) -> ProducerLog {
        self.log.borrow().clone()
    }
}

fn list_tree(root: &Path, prefix: &str, into: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(root)
        .expect("read the projected tree")
        .map(|entry| entry.expect("a directory entry"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type().expect("a file type").is_dir() {
            list_tree(&entry.path(), &path, into);
        } else {
            into.push(path);
        }
    }
}

impl V1GuestProducer for FakeProducer {
    fn assemble(
        &self,
        projected_source: &Path,
        spec: &RootfsBuildSpecV1,
        pinned_base_ref: &str,
        image_ref: &str,
    ) -> Result<AssembledGuestImage, String> {
        let mut guest_files = Vec::new();
        list_tree(projected_source, "", &mut guest_files);

        // The image the guest boots is a function of what went into it. A real
        // build has that property; a double that ignored the source would make
        // every identity-follows-the-source test vacuous.
        let mut hasher = blake3::Hasher::new();
        for path in &guest_files {
            hasher.update(path.as_bytes());
            hasher.update(&std::fs::read(projected_source.join(path)).expect("read a guest file"));
        }
        hasher.update(pinned_base_ref.as_bytes());
        *self.image_bytes.borrow_mut() = hasher.finalize().as_bytes().to_vec();

        let mut log = self.log.borrow_mut();
        log.guest_files = guest_files;
        log.pinned_base_refs.push(pinned_base_ref.to_string());
        log.assembled_argv.push(spec.resolved_argv.clone());
        Ok(AssembledGuestImage::adopt(image_ref.to_string()))
    }

    fn measure_target(&self, image_ref: &str) -> Result<ResolvedTargetContract, String> {
        self.log
            .borrow_mut()
            .measured_images
            .push(image_ref.to_string());
        (self.target)(image_ref)
    }

    fn resolve_runtime(&self, image_ref: &str) -> Result<ResolvedRuntimeArtifact, String> {
        self.log
            .borrow_mut()
            .resolved_images
            .push(image_ref.to_string());
        (self.runtime)(image_ref)
    }

    /// Writes a rootfs whose CONTENT is a function of the projection it was
    /// handed — the property the lane's digest depends on, and the reason this
    /// double cannot make an identity-follows-the-source test vacuous.
    fn export_rootfs(
        &self,
        _image: AssembledGuestImage,
        spec: &RootfsBuildSpecV1,
        rootfs_dir: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(rootfs_dir.join("app")).map_err(|e| e.to_string())?;
        std::fs::write(
            rootfs_dir.join("app/payload"),
            self.image_bytes.borrow().as_slice(),
        )
        .map_err(|e| e.to_string())?;
        std::fs::write(
            rootfs_dir.join("sbin-init"),
            spec.resolved_argv.join("\u{0}"),
        )
        .map_err(|e| e.to_string())?;
        // A wall-clock mtime, as a real export leaves: the digest must ignore
        // it, and a double that wrote a constant could not show that.
        Ok(())
    }

    fn pack_rootfs(
        &self,
        rootfs_dir: &Path,
        out: &Path,
        _size_mib: u64,
        filesystem_uuid: &str,
    ) -> Result<u64, String> {
        self.log
            .borrow_mut()
            .filesystem_uuids
            .push(filesystem_uuid.to_string());
        let bytes = std::fs::read(rootfs_dir.join("app/payload")).map_err(|e| e.to_string())?;
        std::fs::write(out, &bytes).map_err(|error| error.to_string())?;
        self.log.borrow_mut().packed = true;
        Ok(bytes.len() as u64)
    }

    fn discard(&self, image: AssembledGuestImage) {
        self.log
            .borrow_mut()
            .discarded
            .push(image.image_ref().to_string());
    }
}

/// A workspace on disk, plus the scratch directories a build needs.
struct Workspace {
    dir: TempDir,
    work: TempDir,
    out: TempDir,
}

impl Workspace {
    fn new(manifest: &str) -> Self {
        let workspace = Self {
            dir: TempDir::new().expect("workspace"),
            work: TempDir::new().expect("work root"),
            out: TempDir::new().expect("output root"),
        };
        workspace.write("capsule.toml", manifest);
        workspace.write("app.py", "print('hello')\n");
        workspace
    }

    fn write(&self, rel: &str, contents: &str) -> &Self {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create a parent directory");
        }
        std::fs::write(path, contents).expect("write a workspace file");
        self
    }

    fn guest_image_path(&self) -> PathBuf {
        self.out.path().join("guest.img")
    }

    fn build(&self, producer: &dyn V1GuestProducer) -> Result<V1BuildOutcome, V1BuildError> {
        // A fresh work root per build: the lane refuses a dirty projection
        // destination, and a test that builds twice must not trip over the
        // previous run's scratch.
        let work = self.work.path().join(format!(
            "run-{}",
            std::fs::read_dir(self.work.path())
                .map(std::iter::Iterator::count)
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&work).expect("create a per-run work root");
        let guest_image_path = self.guest_image_path();
        run(
            V1BuildRequest {
                workspace_root: self.dir.path(),
                pinned_source_archive: None,
                work_root: &work,
                guest_image_path: &guest_image_path,
                rootfs_size_mib: 512,
                image_ref: "ato-v1-test",
            },
            producer,
        )
    }

    fn lock(&self) -> CapsuleLock {
        capsule_lock::load_verified_from_path(&self.dir.path().join("capsule.lock"))
            .expect("the published lock verifies")
    }
}

/// The minimal capsule the Step-4 subset admits: a python source tree serving a
/// web surface, with an acceptance command.
fn minimal_manifest(extra: &str) -> String {
    format!(
        r#"
schema_version = "1"
name = "demo"
version = "0.1.0"

[run]
command = ["python3", "app.py"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["curl", "-fsS", "http://127.0.0.1:8080/"]
{extra}
"#
    )
}

// ── The lane end to end ──────────────────────────────────────────────────────

/// The whole point of 5-3: a v1 workspace goes in, and a `capsule.lock`
/// carrying an execution identity minted from real measurements comes out —
/// verified by reading it back off disk, not by trusting the value in memory.
/// A pinned build projects the ARCHIVE it was handed, not the workspace it
/// reads the manifest from.
///
/// The two are the same tree on a builder host — the workspace IS the
/// extraction — so the only way to show which one the identity came from is to
/// make them differ. If the lane ever went back to re-freezing the workspace,
/// this is the test that fails: the identity would follow the mutated file
/// instead of the bytes that were proved.
#[test]
fn a_pinned_archive_is_what_the_identity_is_minted_over() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let producer = FakeProducer::healthy();

    // Freeze the workspace, then change it. From here the archive and the
    // workspace disagree about `app.py`.
    let frozen = TempDir::new().expect("archive dir");
    let archive = frozen.path().join("pinned.tar.zst");
    capsule::blob::materialize_source_archive(workspace.dir.path(), &archive)
        .expect("freeze the workspace");
    workspace.write("app.py", "print('a different program')\n");

    let unpinned_work = workspace.work.path().join("unpinned");
    std::fs::create_dir_all(&unpinned_work).expect("work root");
    let unpinned = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: None,
            work_root: &unpinned_work,
            guest_image_path: &workspace.out.path().join("unpinned.img"),
            rootfs_size_mib: 512,
            image_ref: "ato-v1-test-unpinned",
        },
        &producer,
    )
    .expect("the workspace build completes");

    let pinned_work = workspace.work.path().join("pinned");
    std::fs::create_dir_all(&pinned_work).expect("work root");
    let pinned = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: Some(&archive),
            work_root: &pinned_work,
            guest_image_path: &workspace.out.path().join("pinned.img"),
            rootfs_size_mib: 512,
            image_ref: "ato-v1-test-pinned",
        },
        &producer,
    )
    .expect("the pinned build completes");

    assert_ne!(
        pinned.source_digest, unpinned.source_digest,
        "a pinned build that agreed with the mutated workspace would be minting \
         its identity over source nothing proved"
    );
    assert_ne!(pinned.execution_id, unpinned.execution_id);
}

#[test]
fn effective_manifest_run_command_drives_the_guest_and_lock() {
    let original = minimal_manifest("").replace(
        r#"command = ["python3", "app.py"]"#,
        r#"command = ["python3", "app.py", "--original"]"#,
    );
    let edited = original.replace("--original", "--edited");
    let workspace = Workspace::new(&original);
    let frozen = TempDir::new().expect("archive dir");
    let archive = frozen.path().join("pinned.tar.zst");
    capsule::blob::materialize_source_archive(workspace.dir.path(), &archive)
        .expect("freeze original source");
    workspace.write("capsule.toml", &edited);

    let work = workspace.work.path().join("effective-manifest");
    std::fs::create_dir_all(&work).expect("work root");
    let outcome = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: Some(&archive),
            work_root: &work,
            guest_image_path: &workspace.out.path().join("effective.img"),
            rootfs_size_mib: 512,
            image_ref: "ato-v1-effective-manifest",
        },
        &FakeProducer::healthy(),
    )
    .expect("effective manifest build");

    assert_eq!(
        outcome.authored_argv,
        ["python3", "app.py", "--edited"],
        "the guest recipe must use the edited Effective Manifest"
    );
    let manifest = CapsuleManifestV1::from_toml(&edited).expect("edited manifest remains valid");
    assert_eq!(
        workspace
            .lock()
            .manifest
            .expect("manifest lock section")
            .normalized_digest,
        manifest
            .normalized_digest()
            .expect("edited manifest digest")
    );
}

#[test]
fn a_v1_build_mints_publishes_and_reads_back() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let producer = FakeProducer::healthy();

    let outcome = workspace.build(&producer).expect("the v1 build completes");

    assert!(outcome.trusted_load_verified);
    assert!(outcome.execution_id.starts_with("blake3:"));
    assert_eq!(outcome.lock_path, workspace.dir.path().join("capsule.lock"));
    assert!(workspace.guest_image_path().exists());
    assert!(outcome.guest_image_bytes > 0);

    // The lock reads back through the trusted path, and its envelope is the one
    // that was minted.
    let envelope = workspace
        .lock()
        .execution_contract
        .expect("the lock carries an execution contract");
    assert_eq!(envelope.execution_id.as_str(), outcome.execution_id);

    // And the id is a function of the contract, recomputed from what was read.
    assert_eq!(
        envelope
            .execution_contract
            .compute_execution_id()
            .expect("recompute")
            .as_str(),
        outcome.execution_id
    );

    // The producer really ran, in the documented order.
    let log = producer.log();
    assert_eq!(log.resolved_images, [PYTHON_SLIM]);
    assert_eq!(log.measured_images, ["ato-v1-test"]);
    assert!(log.packed);
    assert!(log.discarded.is_empty());
}

/// The identity is not a self-assertion: it names measurements, and the
/// contract that comes back carries every one of them.
#[test]
fn the_published_contract_carries_the_measured_facets() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let outcome = workspace
        .build(&FakeProducer::healthy())
        .expect("the v1 build completes");
    let contract = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract;

    assert_eq!(contract.target, linux_gnu_x86_64());
    assert_eq!(contract.runtime.kind, "python");
    assert_eq!(contract.runtime.digest, digest('c'));
    assert_eq!(contract.launch.argv, ["python3", "app.py"]);
    assert_eq!(contract.launch.cwd.as_str(), "/app");
    assert_eq!(contract.source.digest.to_string(), outcome.source_digest);
    // The composed view is the image that was packed, hashed off the file.
    // The view digest names the guest's CONTENTS, not the packed image's bytes.
    assert_ne!(
        contract.filesystem.view_digest.to_string(),
        "",
        "a view digest is committed"
    );
    assert_eq!(
        contract.filesystem.view_digest, contract.filesystem.readonly_layers[0],
        "one image, so the composed view and the only layer are the same"
    );
}

// ── The projection is what the guest gets ────────────────────────────────────

/// The guest is built from the PROJECTION, not the checkout. This is the fix
/// that makes `source.digest` mean anything: hashing the projection while
/// copying the checkout would commit a digest for a tree the guest does not
/// have, off by exactly the control files.
#[test]
fn the_guest_receives_the_projection_not_the_checkout() {
    let workspace = Workspace::new(&minimal_manifest(""));
    workspace.write("lib/util.py", "X = 1\n");
    let producer = FakeProducer::healthy();
    workspace.build(&producer).expect("the v1 build completes");

    let guest_files = producer.log().guest_files;
    assert_eq!(guest_files, ["app.py", "lib/util.py"]);
    assert!(
        !guest_files.iter().any(|path| path == "capsule.toml"),
        "the manifest is a control file, not program source: {guest_files:?}"
    );
    // Non-vacuous: the checkout it was projected from does have one.
    assert!(workspace.dir.path().join("capsule.toml").exists());
}

/// A change a guest would see moves the identity; a change to a control file
/// does not. Both directions, because only asserting the first would pass for
/// an implementation that hashed the whole checkout.
#[test]
fn the_identity_follows_the_guest_tree_not_the_control_files() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let baseline = workspace
        .build(&FakeProducer::healthy())
        .expect("build")
        .execution_id;

    // The lock the previous build wrote is now a control file in this tree —
    // withheld from the projection, so building again is the same execution.
    let again = workspace
        .build(&FakeProducer::healthy())
        .expect("build again")
        .execution_id;
    assert_eq!(baseline, again, "a lock in the tree is not program source");

    workspace.write("app.py", "print('changed')\n");
    let changed = workspace
        .build(&FakeProducer::healthy())
        .expect("build after a source change")
        .execution_id;
    assert_ne!(baseline, changed);
}

/// A Git working tree is not a source a v1 identity can be minted from: its
/// `.git` is neither a control file the projection may withhold nor content
/// whose bytes are reproducible. The refusal is explicit, not a fallback.
#[test]
fn a_git_working_tree_cannot_mint_an_identity() {
    let workspace = Workspace::new(&minimal_manifest(""));
    workspace.write(".git/config", "[core]\n");

    let error = workspace
        .build(&FakeProducer::healthy())
        .expect_err("a working tree is refused");
    assert!(
        matches!(error, V1BuildError::SourceNotPinnable { .. }),
        "{error:?}"
    );
    assert!(!workspace.dir.path().join("capsule.lock").exists());
}

// ── Exact argv ───────────────────────────────────────────────────────────────

/// v1 argv is exact. A word with a space, a quote, a shell metacharacter and a
/// non-ASCII word all survive into the contract as separate arguments — the
/// boundaries the author wrote, not the ones a shell would rediscover from a
/// joined string.
#[test]
fn exact_argv_survives_into_the_published_lock() {
    let argv = ["python3", "my app.py", "it's", "--filter=a|b>c", "café"];
    let command = argv
        .iter()
        .map(|word| format!("{word:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = minimal_manifest("").replace(
        r#"command = ["python3", "app.py"]"#,
        &format!("command = [{command}]"),
    );

    let workspace = Workspace::new(&manifest);
    let producer = FakeProducer::healthy();
    workspace.build(&producer).expect("the v1 build completes");

    let contract = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract;
    assert_eq!(contract.launch.argv, argv);
    // And the recipe handed the producer the same list — the contract is not
    // describing a launch the build did not configure.
    assert_eq!(producer.log().assembled_argv, [argv.to_vec()]);
}

/// The v0.3 bare-`.py` rewrite must not reach v1. An author who writes
/// `["app.py"]` gets `["app.py"]`, because inventing `python3` would put a word
/// in the Execution Identity that nobody wrote.
#[test]
fn a_bare_python_file_is_not_rewritten_on_the_v1_lane() {
    let manifest = minimal_manifest("").replace(
        r#"command = ["python3", "app.py"]"#,
        r#"command = ["app.py"]"#,
    );
    let workspace = Workspace::new(&manifest);
    workspace.build(&FakeProducer::healthy()).expect("build");

    let contract = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract;
    assert_eq!(contract.launch.argv, ["app.py"]);
}

/// The working directory is where the BUILD put the source, and the same
/// constant the generated Dockerfile and init agree on. It is a guest path, not
/// a host one.
#[test]
fn the_working_directory_is_where_the_build_put_the_source() {
    let workspace = Workspace::new(&minimal_manifest(""));
    workspace.build(&FakeProducer::healthy()).expect("build");

    let cwd = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract
        .launch
        .cwd;
    assert_eq!(cwd.as_str(), V1_GUEST_WORKING_DIRECTORY);
    assert!(cwd.as_str().starts_with('/'));
    assert!(
        !cwd.as_str()
            .contains(&*workspace.dir.path().to_string_lossy()),
        "a host path must never become the guest working directory"
    );
}

/// An empty `[run] command` never reaches the contract: the recipe refuses it,
/// so no observation is taken and no lock is written.
#[test]
fn an_empty_argv_is_refused_before_anything_is_built() {
    let manifest =
        minimal_manifest("").replace(r#"command = ["python3", "app.py"]"#, "command = []");
    let workspace = Workspace::new(&manifest);
    let producer = FakeProducer::healthy();

    let error = workspace
        .build(&producer)
        .expect_err("an empty argv is refused");
    assert!(
        matches!(error, V1BuildError::RecipeDerivationFailed { .. }),
        "{error:?}"
    );
    assert!(!producer.log().packed);
    assert!(!workspace.dir.path().join("capsule.lock").exists());
}

/// An empty WORD inside an argv is refused too, and refused early.
///
/// `launch.argv` must be resolved, and the contract counts an empty string as
/// unresolved — so a capsule declaring one could be built but never minted. The
/// recipe refuses it instead, before a registry round trip and an image build
/// are spent on something that has no identity, and while the message can still
/// point at the manifest line rather than at a contract field.
#[test]
fn an_empty_word_inside_the_argv_is_refused_before_anything_is_built() {
    let manifest = minimal_manifest("").replace(
        r#"command = ["python3", "app.py"]"#,
        r#"command = ["python3", "", "app.py"]"#,
    );
    let workspace = Workspace::new(&manifest);
    let producer = FakeProducer::healthy();

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::RecipeDerivationFailed { .. }),
        "{error:?}"
    );
    assert!(format!("{error}").contains("argv[1] is empty"), "{error}");
    assert!(producer.log().resolved_images.is_empty());
}

// ── The runtime artifact ─────────────────────────────────────────────────────

/// The image is assembled `FROM` the digest-pinned reference the lane resolved,
/// and the resolution happens BEFORE the build — so the bytes the contract
/// records are the bytes the guest was built on.
#[test]
fn the_guest_is_assembled_from_the_pinned_reference() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let producer = FakeProducer::healthy();
    workspace.build(&producer).expect("build");

    let log = producer.log();
    assert_eq!(log.pinned_base_refs, [pinned_ref(PYTHON_SLIM, 'c')]);
    assert!(
        !log.pinned_base_refs.iter().any(|base| base == PYTHON_SLIM),
        "the mutable tag must not be what the guest was built from"
    );
}

/// A resolution that is not pinned to a digest cannot be an identity input: a
/// tag names whatever the registry serves next, which is not a measurement.
#[test]
fn an_unpinned_runtime_resolution_is_refused() {
    let producer = FakeProducer::resolving(|image_ref| {
        Ok(ResolvedRuntimeArtifact {
            original_ref: image_ref.to_string(),
            resolved_ref: image_ref.to_string(),
            digest: digest('c'),
        })
    });
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::RuntimeArtifactResolutionFailed { .. }),
        "{error:?}"
    );
    assert!(format!("{error}").contains("not pinned"), "{error}");
}

/// A resolution that answers for a different image than the recipe asked for
/// would make the contract record a runtime the build did not use.
#[test]
fn a_resolution_for_another_image_is_refused() {
    let producer = FakeProducer::resolving(|_| {
        Ok(ResolvedRuntimeArtifact {
            original_ref: "node:20-slim".into(),
            resolved_ref: pinned_ref("node", 'd'),
            digest: digest('d'),
        })
    });
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(format!("{error}").contains("node:20-slim"), "{error}");
    assert!(!workspace.dir.path().join("capsule.lock").exists());
}

/// A registry that cannot resolve the base image stops the build there, named
/// as a resolution failure rather than as "the build broke".
#[test]
fn an_unresolvable_runtime_stops_the_build_at_resolution() {
    let producer = FakeProducer::resolving(|_| Err("no such image".into()));
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::RuntimeArtifactResolutionFailed { .. }),
        "{error:?}"
    );
    assert!(!producer.log().packed);
}

// ── The guest target ─────────────────────────────────────────────────────────

/// The target is the GUEST's, measured off the assembled image. A builder host
/// that is an arm64 Mac and a guest that is linux/x86_64 are routinely
/// different machines, and the contract must say which one it means.
#[test]
fn the_target_is_the_guests_and_not_this_hosts() {
    let workspace = Workspace::new(&minimal_manifest(""));
    // Deliberately not this host's architecture, whichever host runs the suite.
    let architecture = if std::env::consts::ARCH == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    };
    let guest = ResolvedTargetContract {
        os: "linux".into(),
        architecture: architecture.into(),
        abi: "musl".into(),
        libc: Some("musl".into()),
        observable_features: BTreeMap::new(),
    };
    let expected = guest.clone();
    let producer = FakeProducer::measuring(move |_| Ok(guest.clone()));

    let outcome = workspace.build(&producer).expect("build");
    assert_eq!(outcome.target, expected);
    assert_eq!(
        workspace
            .lock()
            .execution_contract
            .unwrap()
            .execution_contract
            .target,
        expected
    );
    assert_eq!(
        producer.log().measured_images,
        ["ato-v1-test"],
        "the measurement subject is the assembled image, not the host"
    );
    assert_ne!(expected.architecture.as_str(), std::env::consts::ARCH);
}

/// A libc the probe cannot classify has no honest ABI, so the build is refused
/// rather than defaulted.
#[test]
fn an_unclassifiable_libc_refuses_the_build() {
    let producer = FakeProducer::measuring(|image_ref| {
        Err(format!("image {image_ref:?} reported libc \"uclibc\""))
    });
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::GuestTargetMeasurementFailed { .. }),
        "{error:?}"
    );
    assert!(!workspace.dir.path().join("capsule.lock").exists());
}

/// An ABI that does not follow from the measured libc is not resolved, whatever
/// produced it — the two have to describe one machine.
#[test]
fn an_abi_that_contradicts_the_measured_libc_is_refused() {
    let producer = FakeProducer::measuring(|_| {
        Ok(ResolvedTargetContract {
            abi: "gnu".into(),
            libc: Some("musl".into()),
            ..linux_gnu_x86_64()
        })
    });
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::ObservationConflict { .. }),
        "{error:?}"
    );
}

/// A non-Linux guest is outside every recipe this lane has.
#[test]
fn a_non_linux_guest_is_refused() {
    let producer = FakeProducer::measuring(|_| {
        Ok(ResolvedTargetContract {
            os: "windows".into(),
            ..linux_gnu_x86_64()
        })
    });
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = workspace.build(&producer).expect_err("refused");
    assert!(format!("{error}").contains("windows"), "{error}");
}

/// A failure between assembly and packing must not leak the image: nothing else
/// will ever refer to it, and the builder host would accumulate one per failed
/// build.
#[test]
fn an_assembled_image_is_discarded_when_measurement_fails() {
    let producer = FakeProducer::measuring(|_| Err("probe failed".into()));
    let workspace = Workspace::new(&minimal_manifest(""));

    workspace.build(&producer).expect_err("refused");
    assert_eq!(producer.log().discarded, ["ato-v1-test"]);
}

// ── Publishing the lock ──────────────────────────────────────────────────────

/// A workspace already carrying the deprecated alias keeps it. Writing
/// `capsule.lock` beside an `ato.lock.json` would give the workspace two locks,
/// which the resolver then refuses to read at all.
#[test]
fn the_lock_keeps_the_name_the_workspace_already_uses() {
    let workspace = Workspace::new(&minimal_manifest(""));
    capsule_lock::write_pretty_to_path(
        &CapsuleLock::default(),
        &workspace.dir.path().join("ato.lock.json"),
    )
    .expect("seed a lock under the deprecated alias");

    let outcome = workspace.build(&FakeProducer::healthy()).expect("build");
    assert_eq!(
        outcome.lock_path,
        workspace.dir.path().join("ato.lock.json")
    );
    assert!(!workspace.dir.path().join("capsule.lock").exists());

    // And which name was held out is identity-bearing: the same program source
    // spelling its lock the other way is a different projection.
    let canonical = Workspace::new(&minimal_manifest(""));
    canonical.build(&FakeProducer::healthy()).expect("build");
    assert_ne!(
        outcome.execution_id,
        canonical
            .lock()
            .execution_contract
            .unwrap()
            .execution_id
            .as_str()
            .to_string(),
        "the withheld lock NAME is part of the projection"
    );
}

/// Sections of an existing lock that this lane does not own survive the write.
#[test]
fn publishing_preserves_the_rest_of_an_existing_lock() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let seed = CapsuleLock {
        generated_at: Some("2026-01-01T00:00:00Z".to_string()),
        ..CapsuleLock::default()
    };
    capsule_lock::write_pretty_to_path(&seed, &workspace.dir.path().join("capsule.lock"))
        .expect("seed a lock");

    workspace.build(&FakeProducer::healthy()).expect("build");
    assert_eq!(
        workspace.lock().generated_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
}

/// An existing lock that does not verify is not a base to build on. Overwriting
/// it would silently repair a tampered file; keeping its sections would carry
/// whatever is wrong with it into the new one.
#[test]
fn an_existing_unverifiable_lock_refuses_the_build() {
    let workspace = Workspace::new(&minimal_manifest(""));
    // A lock whose `lock_id` does not match its own canonical projection.
    workspace.write(
        "capsule.lock",
        &format!(
            r#"{{"schema_version":1,"lock_id":"blake3:{}"}}"#,
            "a".repeat(64)
        ),
    );

    let error = workspace
        .build(&FakeProducer::healthy())
        .expect_err("refused");
    assert!(
        matches!(error, V1BuildError::LockPersistFailed { .. }),
        "{error:?}"
    );
    assert!(
        format!("{error}").contains("not a base to build on"),
        "{error}"
    );
}

/// A capsule with an authored `[env]` value publishes the D5 section the lock's
/// own read path requires. Without it the envelope would be written and then
/// fail its own trusted-load, which is exactly the failure step 11 exists to
/// catch — so this test is the one that proves the lane does not produce it.
#[test]
fn an_authored_env_value_is_published_so_the_lock_reads_back() {
    let workspace = Workspace::new(&minimal_manifest("\n[env]\nNODE_ENV = \"production\"\n"));
    workspace.build(&FakeProducer::healthy()).expect("build");

    let lock = workspace.lock();
    let launch = lock.launch.clone().expect("the D5 launch section");
    assert_eq!(launch.environment.len(), 1);
    assert_eq!(launch.environment[0].name, "NODE_ENV");

    // The identity commits the same variable, with the same value digest.
    let committed = &lock
        .execution_contract
        .unwrap()
        .execution_contract
        .launch
        .environment;
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].name, "NODE_ENV");
    assert_eq!(
        committed[0].value_digest.to_string(),
        launch.environment[0].value_digest
    );
}

/// An `[env]` name that reads as a secret is refused when the manifest is read,
/// which is before anything is resolved, built or written.
///
/// The refusal belongs to the authoring surface rather than to this lane: an
/// `[env]` value is committed by the identity in the clear, so the gate has to
/// be wherever a v1 manifest is parsed, not only wherever one is built. Left to
/// the read-back it would surface as a complaint about a lock, several minutes
/// and one container build later.
#[test]
fn a_secret_named_env_value_is_refused_before_anything_is_built() {
    let workspace = Workspace::new(&minimal_manifest("\n[env]\nAPI_TOKEN = \"t\"\n"));
    let producer = FakeProducer::healthy();

    let error = workspace.build(&producer).expect_err("refused");
    assert!(
        matches!(error, V1BuildError::RecipeDerivationFailed { .. }),
        "{error:?}"
    );
    // And it names the fix, not just the problem.
    assert!(format!("{error}").contains("kind = \"secret\""), "{error}");
    assert!(producer.log().resolved_images.is_empty());
    assert!(!producer.log().packed);
}

/// A capsule outside the Step-4 subset never reaches a measurement.
#[test]
fn a_capsule_outside_the_subset_is_refused_before_measuring() {
    let workspace = Workspace::new(&minimal_manifest("\n[tools]\npython = \"3.12\"\n"));
    let producer = FakeProducer::healthy();

    let error = workspace.build(&producer).expect_err("refused");
    assert!(format!("{error}").contains("[tools]"), "{error}");
    assert!(producer.log().resolved_images.is_empty());
}

// ── The read-back ────────────────────────────────────────────────────────────

/// A lock whose bytes are mutated after publication does not read back. This is
/// the property the atomic write and the trusted load exist to give; asserting
/// it here keeps the lane honest about depending on it.
#[test]
fn a_mutated_published_lock_does_not_read_back() {
    let workspace = Workspace::new(&minimal_manifest(""));
    workspace.build(&FakeProducer::healthy()).expect("build");

    let lock_path = workspace.dir.path().join("capsule.lock");
    let raw = std::fs::read_to_string(&lock_path).expect("read the lock");
    let mutated = raw.replace("\"python3\"", "\"python2\"");
    assert_ne!(raw, mutated, "the mutation must actually change something");
    std::fs::write(&lock_path, mutated).expect("write the mutated lock");

    capsule_lock::load_verified_from_path(&lock_path).expect_err("a mutated lock does not verify");
}

/// The read-back compares field by field, so a lock that verifies internally
/// but describes another execution is caught and NAMED — a stale lock left by
/// an earlier run would pass an id-only check against itself.
#[test]
fn a_persisted_envelope_describing_another_execution_is_named_and_refused() {
    let one = Workspace::new(&minimal_manifest(""));
    one.build(&FakeProducer::healthy()).expect("build");
    let mine = one.lock().execution_contract.unwrap();

    let other = Workspace::new(&minimal_manifest(""));
    other.write("app.py", "print('other')\n");
    other.build(&FakeProducer::healthy()).expect("build");
    let theirs = other.lock().execution_contract.unwrap();

    assert_ne!(mine.execution_id, theirs.execution_id);
    let error = compare_persisted_to_minted(Path::new("capsule.lock"), &mine, &theirs)
        .expect_err("a different execution is refused");
    match error {
        V1BuildError::PersistedEnvelopeMismatch { field, .. } => {
            assert_eq!(field, "source.digest", "the first field that differs");
        }
        other => panic!("{other:?}"),
    }

    // Identical envelopes agree — otherwise the assertion above would pass for
    // a comparison that always fails.
    compare_persisted_to_minted(Path::new("capsule.lock"), &mine, &mine)
        .expect("an envelope agrees with itself");
}

/// A lock that was written but does not describe this build is taken back, and
/// the workspace is left exactly as it was found.
///
/// Both directions matter. With no previous lock the file is removed, because
/// leaving one that verifies against its own contract while describing another
/// execution is undetectable downstream. With a previous lock the EXACT bytes
/// go back: `persist_execution_contract` merges into the existing file, so a
/// plain removal would take the caller's other sections with it — sections this
/// lane neither owns nor can reconstruct.
#[test]
fn an_unverifiable_lock_is_taken_back_without_destroying_the_previous_one() {
    let directory = TempDir::new().expect("tempdir");
    let lock_path = directory.path().join("capsule.lock");
    let cause = || V1BuildError::PersistedEnvelopeMismatch {
        path: lock_path.clone(),
        field: "target",
        minted: "linux/x86_64/gnu".into(),
        persisted: "linux/aarch64/musl".into(),
    };

    // No previous lock: the file this lane wrote is removed.
    std::fs::write(&lock_path, "{}").expect("write");
    let returned = unpublish(&lock_path, None, cause());
    assert!(!lock_path.exists(), "the unverifiable lock is gone");
    assert!(
        matches!(returned, V1BuildError::PersistedEnvelopeMismatch { .. }),
        "the original cause is what the caller sees: {returned:?}"
    );

    // A previous lock: its bytes come back verbatim, not just its existence.
    let previous = br#"{"schema_version":1,"resolution":{"kept":true}}"#;
    std::fs::write(&lock_path, b"this build's merged output").expect("write");
    unpublish(&lock_path, Some(previous), cause());
    assert_eq!(
        std::fs::read(&lock_path).expect("the previous lock is back"),
        previous
    );
}

/// A build whose read-back fails must not cost the workspace the lock it had.
///
/// The end-to-end version of the property above: the lane merges into the
/// existing lock, so the failure path has to restore rather than remove.
#[test]
fn a_failed_read_back_leaves_the_workspace_its_previous_lock() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let lock_path = workspace.dir.path().join("capsule.lock");

    // A lock this lane did not write, carrying a section it does not own.
    let seed = CapsuleLock {
        generated_at: Some("2026-01-01T00:00:00Z".to_string()),
        ..CapsuleLock::default()
    };
    capsule_lock::write_pretty_to_path(&seed, &lock_path).expect("seed a lock");
    let before = std::fs::read(&lock_path).expect("read the seeded lock");

    let cause = V1BuildError::TrustedLoadFailed {
        path: lock_path.clone(),
        reason: "simulated".into(),
    };
    // Stand in for the merged write the lane performs before its read-back.
    std::fs::write(&lock_path, b"merged output that will not verify").expect("write");
    unpublish(&lock_path, Some(&before), cause);

    assert_eq!(std::fs::read(&lock_path).unwrap(), before);
    assert_eq!(
        workspace.lock().generated_at.as_deref(),
        Some("2026-01-01T00:00:00Z"),
        "and it still verifies, so the workspace is genuinely unchanged"
    );
}

// ── Producers and provenance ─────────────────────────────────────────────────

/// The identity commits the guest's CONTENTS, so two exports of the same tree
/// agree however the host stamped them — and a content change still moves it.
///
/// The lane-level counterpart of `guest_filesystem_digest`'s own tests: this
/// one goes through the real `export_rootfs` seam.
#[test]
fn the_view_digest_follows_the_guest_contents() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let view = |outcome_workspace: &Workspace| {
        outcome_workspace
            .lock()
            .execution_contract
            .unwrap()
            .execution_contract
            .filesystem
            .view_digest
    };

    workspace.build(&FakeProducer::healthy()).expect("build");
    let first = view(&workspace);
    workspace.build(&FakeProducer::healthy()).expect("rebuild");
    assert_eq!(first, view(&workspace), "same contents, same digest");

    workspace.write("app.py", "print('changed')\n");
    workspace.build(&FakeProducer::healthy()).expect("build");
    assert_ne!(
        first,
        view(&workspace),
        "different contents, different digest"
    );
}

/// Every facet this lane produces a measurement for names its producer, so a
/// refusal points at the step that did not run instead of at a contract field.
#[test]
fn each_produced_facet_names_the_producer_it_came_from() {
    let produced = [
        ("source.digest", "projection"),
        ("source.projection_digest", "projection"),
        ("target", "measure_guest_target"),
        // Only the DIGEST comes from the registry resolution. The family is
        // decided by the source probe over the projection, and the dynamic
        // contract is built from the family plus the recipe's observed prefix —
        // pointing an operator at the registry for either would be exactly the
        // misdirection this table exists to prevent.
        ("runtime.digest", "resolve_runtime_artifact"),
        ("runtime.kind", "recipe derivation"),
        ("runtime.dynamic_contract_digest", "recipe derivation"),
        ("launch.argv", "launch descriptor"),
        ("launch.cwd", "launch descriptor"),
        ("filesystem.view_digest", "packed guest image"),
        ("filesystem.readonly_layers", "packed guest image"),
    ];
    for (facet, expected) in produced {
        let provenance = facet_provenance(facet);
        assert!(
            provenance.contains(expected),
            "{facet} → {provenance:?}, expected to mention {expected:?}"
        );
    }

    // And the mint's refusal carries it, rather than only the field name.
    let error = mint_error_from_finalization(FinalizationError::UnmeasuredFacet("target"));
    match error {
        V1BuildError::ObservationIncomplete { facet } => {
            assert!(facet.contains("target"), "{facet}");
            assert!(facet.contains("measure_guest_target"), "{facet}");
        }
        other => panic!("{other:?}"),
    }
}

/// The short id is a prefix, never the identity. Truncating in the terminal is
/// only safe if nothing can mistake it for the whole value.
#[test]
fn the_short_execution_id_is_a_recognizable_prefix() {
    let outcome = V1BuildOutcome {
        execution_id: format!("blake3:{}", "a".repeat(64)),
        lock_path: PathBuf::from("capsule.lock"),
        guest_image_path: PathBuf::from("guest.img"),
        guest_image_bytes: 1,
        guest_image_digest: format!("blake3:{}", "d".repeat(64)),
        filesystem_view_digest: format!("blake3:{}", "e".repeat(64)),
        source_digest: format!("sha256:{}", "b".repeat(64)),
        runtime_resolved_ref: pinned_ref(PYTHON_SLIM, 'c'),
        target: linux_gnu_x86_64(),
        trusted_load_verified: true,
        port: 8080,
        authored_argv: vec!["python3".to_string(), "app.py".to_string()],
    };
    assert_eq!(
        outcome.short_execution_id(),
        format!("blake3:{}", "a".repeat(12))
    );
    assert!(
        outcome
            .execution_id
            .starts_with(&outcome.short_execution_id())
    );
    assert_ne!(outcome.short_execution_id(), outcome.execution_id);
}

/// Two builds of one program source mint one `execution_id`.
///
/// This is the property `capsule.lock` depends on, and it holds against the
/// real producer too — the identity commits the guest's CONTENTS, so the
/// wall-clock stamps `mke2fs` puts in the artifact cannot reach it. Proven end
/// to end by the `v1 pack is reproducible` CI job; here it says the lane feeds
/// the mint nothing else that varies between two runs.
#[test]
fn repeated_v1_builds_mint_the_same_execution_id() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let first = workspace.build(&FakeProducer::healthy()).expect("build");
    let second = workspace.build(&FakeProducer::healthy()).expect("rebuild");
    assert_eq!(first.source_digest, second.source_digest);
    assert_eq!(first.execution_id, second.execution_id);
}

/// Nothing about where the checkout lives reaches the identity — not the
/// source digest, and not any other facet.
#[test]
fn nothing_about_the_host_path_reaches_the_identity() {
    let manifest = minimal_manifest("");
    let one = Workspace::new(&manifest);
    let two = Workspace::new(&manifest);
    assert_ne!(one.dir.path(), two.dir.path());

    let first = one.build(&FakeProducer::healthy()).expect("build");
    let second = two.build(&FakeProducer::healthy()).expect("build");
    assert_eq!(first.source_digest, second.source_digest);
    assert_eq!(first.execution_id, second.execution_id);
}

/// A build that never reached the mint leaves no envelope and no artifact
/// behind.
#[test]
fn a_failed_build_publishes_nothing() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let producer = FakeProducer::resolving(|_| Err("registry unreachable".into()));

    workspace.build(&producer).expect_err("refused");
    assert!(!workspace.dir.path().join("capsule.lock").exists());
    assert!(!workspace.guest_image_path().exists());
}

/// The envelope in the lock is minted, never assembled field by field:
/// re-verifying it recomputes the id from the contract and finds it equal.
#[test]
fn the_published_envelope_verifies_itself() {
    let workspace = Workspace::new(&minimal_manifest(""));
    workspace.build(&FakeProducer::healthy()).expect("build");

    let envelope: ExecutionContractEnvelopeV1 = workspace.lock().execution_contract.unwrap();
    envelope.verify().expect("the envelope binds its own id");
    assert!(envelope.capsule_program_id.is_none());
}

/// The withheld set does not depend on whether this workspace has been built
/// before.
///
/// The projection reports what it removed, and on a first build there is no
/// lock to remove — but this lane writes one, so reporting the projection's
/// answer verbatim would make a project's identity a function of its build
/// history. The lock is declared as withheld either way, because it is a
/// control file of this workspace whether or not it exists yet.
#[test]
fn the_withheld_set_does_not_depend_on_build_history() {
    let canonical = MaterializedProgramSource {
        contract: capsule::capsule_program_contract::ProgramSourceContract {
            digest: capsule::capsule_program_contract::ProgramSourceDigest::new([0u8; 32]),
            projection_schema: capsule::capsule_program_contract::ProgramSourceProjectionSchemaV1,
        },
        excluded_control_files: vec!["capsule.toml".into()],
    };
    let after_first_build = MaterializedProgramSource {
        excluded_control_files: vec!["capsule.lock".into(), "capsule.toml".into()],
        ..canonical.clone()
    };

    let lock = Path::new("/w/capsule.lock");
    assert_eq!(
        withheld_control_files(&canonical, lock),
        ["capsule.lock", "capsule.toml"],
        "the lock this build is about to write is declared before it exists"
    );
    assert_eq!(
        withheld_control_files(&after_first_build, lock),
        withheld_control_files(&canonical, lock),
        "and the second build declares the same set as the first"
    );

    // Which NAME is withheld still varies — that is a real difference between
    // two repositories, and the payload exists to record it.
    assert_eq!(
        withheld_control_files(&canonical, Path::new("/w/ato.lock.json")),
        ["ato.lock.json", "capsule.toml"]
    );
}

/// A build output written INSIDE the workspace would be hashed as program
/// source by the next build.
///
/// The archive that freezes the workspace walks everything under it, so a
/// guest image left in the tree becomes part of the next build's
/// `source.digest` — and since the image is itself a function of the source,
/// there is no fixed point: every build would mint a new identity. This is the
/// same feedback the withheld-lock rule closes, arriving through the artifact
/// instead of the lock, and the fixtures elsewhere in this file cannot see it
/// because they write the image to a separate directory.
///
/// The lane refuses the path rather than quietly excluding it: `.ato/` is not
/// on ADR-014 §1's control-file list, and widening that list is a normative
/// change to what a Capsule Program's source IS.
#[test]
fn a_guest_image_path_inside_the_workspace_is_refused() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let inside = workspace.dir.path().join(".ato/build/guest.img");
    std::fs::create_dir_all(inside.parent().unwrap()).expect("create the output directory");

    let error = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: None,
            work_root: workspace.work.path(),
            guest_image_path: &inside,
            rootfs_size_mib: 512,
            image_ref: "ato-v1-test",
        },
        &FakeProducer::healthy(),
    )
    .expect_err("an output inside the source tree is refused");

    assert!(
        matches!(error, V1BuildError::SourceNotPinnable { .. }),
        "{error:?}"
    );
    assert!(
        format!("{error}").contains("inside the workspace"),
        "{error}"
    );
}

/// A relative path is refused rather than resolved against the process CWD.
///
/// Resolving it would make the answer depend on where the user was standing: a
/// path that is outside the workspace from one directory is inside it from
/// another. The hole this closes is a path NONE of whose components exist yet —
/// there is nothing to canonicalize, so an ancestor walk finds no answer and
/// would otherwise fall through to "not inside".
#[test]
fn a_relative_build_path_is_refused() {
    let workspace = Workspace::new(&minimal_manifest(""));

    let error = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: None,
            work_root: workspace.work.path(),
            // Nothing on this path exists, and it is relative — so under the
            // process CWD it could land straight back inside the workspace.
            guest_image_path: Path::new("out/guest.img"),
            rootfs_size_mib: 512,
            image_ref: "ato-v1-test",
        },
        &FakeProducer::healthy(),
    )
    .expect_err("a relative build path is refused");
    assert!(format!("{error}").contains("relative path"), "{error}");
}

/// The scratch directory is refused inside the workspace for the same reason:
/// the frozen archive and the materialized projection would both be hashed as
/// program source.
#[test]
fn a_work_root_inside_the_workspace_is_refused() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let inside = workspace.dir.path().join("build-scratch");
    std::fs::create_dir_all(&inside).expect("create the scratch directory");

    let error = run(
        V1BuildRequest {
            workspace_root: workspace.dir.path(),
            pinned_source_archive: None,
            work_root: &inside,
            guest_image_path: &workspace.guest_image_path(),
            rootfs_size_mib: 512,
            image_ref: "ato-v1-test",
        },
        &FakeProducer::healthy(),
    )
    .expect_err("scratch inside the source tree is refused");
    assert!(
        format!("{error}").contains("inside the workspace"),
        "{error}"
    );
}

/// The lane derives the filesystem UUID from the build's own inputs and hands
/// it to the pack, rather than letting `mke2fs` draw one at random.
///
/// It lands in the packed bytes that `filesystem.view_digest` commits, so a
/// random one would put entropy into the Execution Identity. Stable for one
/// program source, and moved by a change the guest would see.
#[test]
fn the_pack_is_given_a_uuid_derived_from_the_build() {
    let workspace = Workspace::new(&minimal_manifest(""));

    let first = FakeProducer::healthy();
    workspace.build(&first).expect("build");
    let second = FakeProducer::healthy();
    workspace.build(&second).expect("rebuild");

    let uuid = |producer: &FakeProducer| producer.log().filesystem_uuids[0].clone();
    assert_eq!(uuid(&first), uuid(&second), "stable across builds");
    assert_eq!(
        uuid(&first),
        v1_filesystem_uuid(
            &first_source_digest(&workspace),
            &pinned_ref(PYTHON_SLIM, 'c'),
            &["python3".to_string(), "app.py".to_string()],
        ),
        "and it is the derivation, not an unrelated constant"
    );

    // A change the guest would see moves it.
    workspace.write("app.py", "print('changed')\n");
    let third = FakeProducer::healthy();
    workspace
        .build(&third)
        .expect("build after a source change");
    assert_ne!(uuid(&first), uuid(&third));
}

fn first_source_digest(workspace: &Workspace) -> String {
    workspace
        .lock()
        .execution_contract
        .expect("a published contract")
        .execution_contract
        .source
        .digest
        .to_string()
}

/// The packed artifact's digest is RECORDED but never committed.
///
/// The two values are one `ContentDigest` apart and swapping them is the whole
/// bug this design exists to prevent: the artifact digest is not stable across
/// builds, so committing it would make a rebuild a different execution. The
/// receipt still needs it — it names which file this build wrote.
#[test]
fn the_artifact_digest_is_recorded_and_never_committed() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let outcome = workspace.build(&FakeProducer::healthy()).expect("build");
    let contract = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract;

    // The receipt names the file on disk.
    assert_eq!(
        outcome.guest_image_digest,
        measure_guest_artifact(&workspace.guest_image_path())
            .expect("hash the artifact")
            .to_string()
    );

    // The contract commits the CONTENTS, which is a different value.
    assert_eq!(
        outcome.filesystem_view_digest,
        contract.filesystem.view_digest.to_string()
    );
    assert_ne!(
        outcome.guest_image_digest, outcome.filesystem_view_digest,
        "the artifact digest and the view digest must not be the same value"
    );

    // And the artifact digest appears nowhere in the identity preimage.
    let canonical = String::from_utf8(contract.canonical_bytes().expect("canonical")).unwrap();
    let bare = outcome
        .guest_image_digest
        .split_once(':')
        .expect("an algorithm-prefixed digest")
        .1;
    assert!(
        !canonical.contains(bare),
        "the packed artifact's digest reached the execution identity"
    );
}

/// A producer cannot supply the identity-bearing digest.
///
/// Structural rather than asserted at runtime: no method on `V1GuestProducer`
/// returns a digest, so the seam a test replaces has no way to inject one. The
/// lane computes it from the exported tree. This test states the property so
/// that adding such a method has to argue with it.
#[test]
fn no_producer_method_can_supply_the_identity() {
    let workspace = Workspace::new(&minimal_manifest(""));
    let producer = FakeProducer::healthy();
    workspace.build(&producer).expect("build");

    let view = workspace
        .lock()
        .execution_contract
        .unwrap()
        .execution_contract
        .filesystem
        .view_digest;

    // The double never chose this value: it wrote a tree, and the lane digested
    // it. Recomputing from the same tree is the only way to reach the same
    // number, which is what "the producer cannot inject one" means in practice.
    assert!(view.to_string().starts_with("blake3:"));
    assert!(
        producer.log().filesystem_uuids.len() == 1,
        "the producer was asked to pack, not to measure"
    );
}
