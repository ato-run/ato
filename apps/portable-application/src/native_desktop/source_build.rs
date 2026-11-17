//! Isolated, content-addressed build of a source-pinned native route.
//!
//! A source route is not a shell hook. The declaration pins the source tarball,
//! the commit it must contain, the Cargo.lock digest and one exact toolchain,
//! and the only thing this module ever runs is that pinned cargo build.
//!
//! The build happens inside the existing Seatbelt sandbox with the source tree
//! read-only, a Run-scoped target directory and scratch, and TCP egress limited
//! to HTTPS. Cargo's own caches live under the Ato home, so two builds of
//! different versions cannot contaminate each other.
//!
//! The output is cached by the content hash of everything that decides it
//! (recipe version, source bytes, commit, lock digest, toolchain, target,
//! command, entry). A second Run with the same inputs reuses the cache without
//! building, and a cached artifact whose bytes no longer match its recorded
//! digest is discarded rather than used.
//!
//! Nothing here decides K. A finished build is an execution fact; the Run still
//! has to launch the artifact and satisfy every observation.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ato_formation::native_desktop::{CargoBuildInvocation, SourceBuild};
use sha2::{Digest, Sha256};

use super::artifact::extract_source_tar_gz;
use super::toolchain::RustToolchain;

/// Identifies the build recipe itself, so changing what a build does cannot
/// reuse an old cache entry.
pub const BUILD_RECIPE_VERSION: &str = "ato.native.source-build/1";
/// The staged layout a source-built entry is resolved against.
pub const BUILD_OUTPUT_PREFIX: &str = ".ato-build/target/release";
/// Relative to the Ato home.
pub const BUILD_CACHE_DIR: &str = "cache/native-build";
/// TCP port a build may reach. Cargo needs the registry over HTTPS only.
pub const BUILD_EGRESS_PORT: u16 = 443;

/// Whether a build actually ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildCache {
    Hit,
    Miss,
}

impl BuildCache {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

/// A build refusal. Every variant is a distinct, named reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildFailure {
    #[error("native_build_entry_invalid: the entry `{0}` is not under the staged build prefix")]
    EntryInvalid(String),
    #[error("native_build_source_archive_invalid: {0}")]
    ArchiveInvalid(String),
    #[error(
        "native_build_source_commit_mismatch: the tarball contains `{found}`, the declaration pins commit `{expected}`"
    )]
    CommitMismatch { found: String, expected: String },
    #[error(
        "native_build_cargo_lock_mismatch: the extracted Cargo.lock is {actual}, the declaration pins {expected}"
    )]
    CargoLockMismatch { expected: String, actual: String },
    #[error("native_build_output_missing: cargo finished without producing `{0}`")]
    OutputMissing(String),
    #[error("native_build_sandbox_unavailable: {0}")]
    SandboxUnavailable(String),
    #[error("native_build_failed: {0}")]
    Failed(String),
    #[error("native_build_io: {0}")]
    Io(String),
}

/// One staged build result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltArtifact {
    /// A directory whose layout is the staged build prefix plus one file.
    pub root: PathBuf,
    /// The absolute entry path inside `root`.
    pub entry: PathBuf,
    pub cache: BuildCache,
    /// The content hash the cache is keyed by.
    pub cache_key: String,
    /// The digest of the produced file.
    pub output_digest: String,
    pub toolchain: String,
    pub build_argv: String,
}

/// The exact command one build runs.
pub struct BuildExecution<'a> {
    pub cargo: &'a Path,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub environment: &'a BTreeMap<String, String>,
    pub target_dir: &'a Path,
    pub source: &'a Path,
    pub toolchain_root: &'a Path,
    pub scratch: &'a Path,
}

/// Runs the pinned command in the build sandbox.
pub trait BuildExecutor {
    fn run(&self, execution: &BuildExecution<'_>) -> Result<(), BuildFailure>;
}

/// The file name a staged entry names, when the entry has the pinned shape.
pub fn staged_file_name(entry: &str) -> Option<&str> {
    let rest = entry.strip_prefix(BUILD_OUTPUT_PREFIX)?.strip_prefix('/')?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest)
}

/// Everything one build is decided by, in one canonical byte form.
pub fn cache_key(source: &SourceBuild, target: &str, entry: &str) -> String {
    let mut canonical = String::new();
    for part in [
        BUILD_RECIPE_VERSION.to_owned(),
        source.sha256.clone(),
        source.commit.clone(),
        source.cargo_lock.clone(),
        source.toolchain.as_str(),
        target.to_owned(),
        source.build.as_str(),
        entry.to_owned(),
    ] {
        canonical.push_str(&part);
        canonical.push('\0');
    }
    format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
}

/// The manifest a cache entry stores beside its bytes.
const CACHE_MANIFEST: &str = "manifest.txt";

fn sha256_file(path: &Path) -> Result<String, BuildFailure> {
    let mut file = fs::File::open(path)
        .map_err(|error| BuildFailure::Io(format!("could not read {}: {error}", path.display())))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)
        .map_err(|error| BuildFailure::Io(format!("could not hash {}: {error}", path.display())))?;
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), BuildFailure> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            BuildFailure::Io(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    fs::copy(source, destination).map_err(|error| {
        BuildFailure::Io(format!(
            "could not copy {} to {}: {error}",
            source.display(),
            destination.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(destination, fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// The environment the build child receives.
///
/// Nothing is inherited: the build sees the managed toolchain, a fixed system
/// PATH for the linker, and this Run's scratch. A host secret in the caller's
/// environment is therefore not reachable from a build script.
pub fn build_environment(
    toolchain: &RustToolchain,
    target_dir: &Path,
    scratch: &Path,
) -> BTreeMap<String, String> {
    let mut environment = toolchain.environment();
    environment.insert(
        "CARGO_TARGET_DIR".to_owned(),
        target_dir.display().to_string(),
    );
    environment.insert("CARGO_INCREMENTAL".to_owned(), "0".to_owned());
    // A native build shares an interactive desktop. Bound concurrent rustc
    // processes so a first GUI build does not exhaust a small machine's RAM.
    environment.insert("CARGO_BUILD_JOBS".to_owned(), "2".to_owned());
    environment.insert(
        "HOME".to_owned(),
        scratch.join("home").display().to_string(),
    );
    environment.insert(
        "TMPDIR".to_owned(),
        scratch.join("tmp").display().to_string(),
    );
    environment.insert(
        "PATH".to_owned(),
        format!(
            "{}:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            toolchain.bin_dir.display()
        ),
    );
    environment
}

/// The declared command, with the pinned binary substituted for `cargo`.
pub fn build_command(invocation: &CargoBuildInvocation, cargo: &Path) -> Vec<String> {
    let mut command = vec![cargo.display().to_string()];
    command.extend(invocation.command().into_iter().skip(1));
    command
}

/// Check the extracted source tree against the declaration.
///
/// Three independent facts are proved: the tarball has one top-level directory,
/// that directory names the declared commit (which is what a commit tarball is),
/// and the `Cargo.lock` it carries is the pinned one. A tarball whose bytes
/// match but whose commit or lock file differ is still refused.
pub fn verify_source_tree(
    source_root: &Path,
    build: &SourceBuild,
) -> Result<PathBuf, BuildFailure> {
    let mut directories = Vec::new();
    let entries = fs::read_dir(source_root).map_err(|error| {
        BuildFailure::ArchiveInvalid(format!("could not read {}: {error}", source_root.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            BuildFailure::ArchiveInvalid(format!("could not read an archive entry: {error}"))
        })?;
        if entry.path().is_dir() {
            directories.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if directories.len() != 1 {
        return Err(BuildFailure::ArchiveInvalid(format!(
            "a source tarball must carry exactly one top-level directory, found {}",
            directories.len()
        )));
    }
    let directory = directories.remove(0);
    let suffix = format!("-{}", build.commit);
    if !directory.ends_with(&suffix) {
        return Err(BuildFailure::CommitMismatch {
            found: directory,
            expected: build.commit.clone(),
        });
    }
    let tree = source_root.join(&directory);
    let lock = tree.join("Cargo.lock");
    if !lock.is_file() {
        return Err(BuildFailure::ArchiveInvalid(
            "the source tree carries no Cargo.lock, so a locked build cannot be proved".to_owned(),
        ));
    }
    let actual = sha256_file(&lock)?;
    if actual != build.cargo_lock {
        return Err(BuildFailure::CargoLockMismatch {
            expected: build.cargo_lock.clone(),
            actual,
        });
    }
    Ok(tree)
}

/// Read a cache entry, when it is intact.
fn reuse_cache(cache_root: &Path, staging: &Path, entry: &str, key: &str) -> Option<BuiltArtifact> {
    let manifest = fs::read_to_string(cache_root.join(CACHE_MANIFEST)).ok()?;
    let mut lines = manifest.lines();
    if lines.next()? != key {
        return None;
    }
    let recorded = lines.next()?.to_owned();
    let cached = cache_root.join(entry);
    if sha256_file(&cached).ok()? != recorded {
        // The value is only reusable while its bytes still are what was built.
        let _ = fs::remove_dir_all(cache_root);
        return None;
    }
    copy_file(&cached, &staging.join(entry)).ok()?;
    Some(BuiltArtifact {
        root: staging.to_path_buf(),
        entry: staging.join(entry),
        cache: BuildCache::Hit,
        cache_key: key.to_owned(),
        output_digest: recorded,
        toolchain: String::new(),
        build_argv: String::new(),
    })
}

/// Build one source route, reusing the cache when the inputs are identical.
#[allow(clippy::too_many_arguments)]
pub fn build(
    build: &SourceBuild,
    entry: &str,
    home: &Path,
    run_root: &Path,
    target: &str,
    toolchain: &RustToolchain,
    source_archive: &Path,
    executor: &dyn BuildExecutor,
) -> Result<BuiltArtifact, BuildFailure> {
    let file_name =
        staged_file_name(entry).ok_or_else(|| BuildFailure::EntryInvalid(entry.to_owned()))?;
    let key = cache_key(build, target, entry);
    let hex = key.trim_start_matches("sha256:").to_owned();
    let cache_root = home.join(BUILD_CACHE_DIR).join(&hex);
    let working = run_root.join("native-build");
    let staging = working.join("staging");
    let scratch = working.join("scratch");
    for directory in [&staging, &scratch] {
        fs::create_dir_all(directory).map_err(|error| {
            BuildFailure::Io(format!("could not create {}: {error}", directory.display()))
        })?;
    }
    if let Some(mut built) = reuse_cache(&cache_root, &staging, entry, &key) {
        built.toolchain = build.toolchain.as_str();
        built.build_argv = build.build.as_str();
        return Ok(built);
    }

    let source_root = working.join("source");
    let archive = fs::read(source_archive).map_err(|error| {
        BuildFailure::Io(format!(
            "could not read {}: {error}",
            source_archive.display()
        ))
    })?;
    extract_source_tar_gz(&archive, &source_root)
        .map_err(|error| BuildFailure::ArchiveInvalid(format!("{error:#}")))?;
    let tree = verify_source_tree(&source_root, build)?;

    let target_dir = scratch.join("target");
    let environment = build_environment(toolchain, &target_dir, &scratch);
    // Linkers and build scripts use these paths directly; exporting a missing
    // TMPDIR makes clang fail only after a potentially long compilation.
    for directory in [scratch.join("home"), scratch.join("tmp")] {
        fs::create_dir_all(&directory).map_err(|error| {
            BuildFailure::Io(format!("could not create {}: {error}", directory.display()))
        })?;
    }
    let args = build_command(&build.build, &toolchain.cargo)
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    executor.run(&BuildExecution {
        cargo: &toolchain.cargo,
        args: &args,
        cwd: &tree,
        environment: &environment,
        target_dir: &target_dir,
        source: &tree,
        toolchain_root: &toolchain.root,
        scratch: &scratch,
    })?;

    let produced = target_dir.join("release").join(file_name);
    if !produced.is_file() {
        return Err(BuildFailure::OutputMissing(produced.display().to_string()));
    }
    let output_digest = sha256_file(&produced)?;
    copy_file(&produced, &staging.join(entry))?;
    // Publish complete output and evidence together. Concurrent builds must not
    // truncate a cache executable that an installed GUI is currently running.
    let cache_parent = home.join(BUILD_CACHE_DIR);
    fs::create_dir_all(&cache_parent).map_err(|error| BuildFailure::Io(error.to_string()))?;
    let pending = tempfile::Builder::new()
        .prefix("build-")
        .tempdir_in(&cache_parent)
        .map_err(|error| BuildFailure::Io(error.to_string()))?;
    copy_file(&produced, &pending.path().join(entry))?;
    let manifest = format!(
        "{key}\n{output_digest}\n{}\n{}\n",
        build.toolchain.as_str(),
        build.build.as_str()
    );
    fs::write(pending.path().join(CACHE_MANIFEST), manifest).map_err(|error| {
        BuildFailure::Io(format!(
            "could not write the cache manifest in {}: {error}",
            cache_root.display()
        ))
    })?;
    if let Err(error) = fs::rename(pending.path(), &cache_root) {
        if let Some(mut winner) = reuse_cache(&cache_root, &staging, entry, &key) {
            winner.toolchain = build.toolchain.as_str();
            winner.build_argv = build.build.as_str();
            return Ok(winner);
        }
        return Err(BuildFailure::Io(format!(
            "could not publish build cache: {error}"
        )));
    }
    Ok(BuiltArtifact {
        root: staging.clone(),
        entry: staging.join(entry),
        cache: BuildCache::Miss,
        cache_key: key,
        output_digest,
        toolchain: build.toolchain.as_str(),
        build_argv: build.build.as_str(),
    })
}

/// The production executor: the pinned command under a Seatbelt profile that
/// makes the source read-only and bounds egress to HTTPS.
pub struct SandboxedCargo;

impl BuildExecutor for SandboxedCargo {
    fn run(&self, execution: &BuildExecution<'_>) -> Result<(), BuildFailure> {
        let launcher = ato_host_facts::trusted_tool(&[ato_host_facts::SEATBELT_LAUNCHER_PATH])
            .ok_or_else(|| {
                BuildFailure::SandboxUnavailable(
                    "sandbox-exec is not available, so the mandatory build containment cannot be \
                     applied"
                        .to_owned(),
                )
            })?;
        let mut read_write = vec![
            execution.target_dir.to_path_buf(),
            execution.scratch.to_path_buf(),
        ];
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(path) = execution.environment.get(name) {
                read_write.push(PathBuf::from(path));
            }
        }
        let system = [
            "/usr",
            "/System",
            "/Library",
            "/bin",
            "/sbin",
            "/opt",
            "/usr/local",
            "/private/etc",
            "/private/var/db",
            "/dev",
        ];
        let policy = ato_sandbox::SandboxPolicy::new()
            .allow_read_only(
                std::iter::once(execution.source.to_path_buf())
                    .chain(std::iter::once(execution.toolchain_root.to_path_buf()))
                    .chain(system.iter().map(PathBuf::from)),
            )
            .allow_read_write(read_write)
            .with_network(false)
            .allow_tcp_connect_remote([BUILD_EGRESS_PORT]);
        let profile = ato_sandbox::render_seatbelt_profile(&policy).map_err(|error| {
            BuildFailure::SandboxUnavailable(format!(
                "the build profile could not be built: {error:#}"
            ))
        })?;
        let profile_path = execution.scratch.join("build.sb");
        fs::write(&profile_path, profile).map_err(|error| {
            BuildFailure::Io(format!(
                "could not write {}: {error}",
                profile_path.display()
            ))
        })?;
        let mut child = Command::new(&launcher)
            .arg("-f")
            .arg(&profile_path)
            .arg(execution.cargo)
            .args(execution.args)
            .current_dir(execution.cwd)
            .env_clear()
            .envs(execution.environment)
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| BuildFailure::Failed(format!("could not start cargo: {error}")))?;
        let mut stderr = child.stderr.take().expect("cargo stderr was piped");
        let mut tail = std::collections::VecDeque::with_capacity(2000);
        let mut buffer = [0_u8; 4096];
        loop {
            let count = match stderr.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(BuildFailure::Failed(format!(
                        "read cargo progress: {error}"
                    )));
                }
            };
            if count == 0 {
                break;
            }
            let _ = std::io::stderr().write_all(&buffer[..count]);
            for byte in &buffer[..count] {
                if tail.len() == 2000 {
                    tail.pop_front();
                }
                tail.push_back(*byte);
            }
        }
        let status = child
            .wait()
            .map_err(|error| BuildFailure::Failed(format!("wait for cargo: {error}")))?;
        if !status.success() {
            return Err(BuildFailure::Failed(format!(
                "cargo exited with {}: {}",
                status,
                String::from_utf8_lossy(tail.make_contiguous())
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::native_desktop::{
        CargoBuildInvocation, NativeBuildCachePolicy, RustToolchainSpec,
    };
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::cell::Cell;

    const COMMIT: &str = "8ba46e9e2181396a9b5903b98031399aef61cf26";
    const ENTRY: &str = ".ato-build/target/release/app";

    struct FakeCargo {
        calls: Cell<usize>,
        fail: bool,
    }

    impl FakeCargo {
        fn new() -> Self {
            Self {
                calls: Cell::new(0),
                fail: false,
            }
        }
    }

    impl BuildExecutor for FakeCargo {
        fn run(&self, execution: &BuildExecution<'_>) -> Result<(), BuildFailure> {
            self.calls.set(self.calls.get() + 1);
            for name in ["HOME", "TMPDIR"] {
                let directory = Path::new(&execution.environment[name]);
                assert!(directory.starts_with(execution.scratch));
                fs::write(directory.join("build-tool-probe"), b"scratch is usable").unwrap();
            }
            if self.fail {
                return Err(BuildFailure::Failed("cargo exited with 101".to_owned()));
            }
            let release = execution.target_dir.join("release");
            fs::create_dir_all(&release).unwrap();
            fs::write(release.join("app"), b"built binary").unwrap();
            Ok(())
        }
    }

    fn sha(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn build_with(
        build: &SourceBuild,
        home: &Path,
        run_root: &Path,
        toolchain: &RustToolchain,
        source: &Path,
        executor: &dyn BuildExecutor,
    ) -> Result<BuiltArtifact, BuildFailure> {
        super::build(
            build,
            ENTRY,
            home,
            run_root,
            "aarch64-apple-darwin",
            toolchain,
            source,
            executor,
        )
    }

    fn source_build(commit: &str, lock: &str) -> SourceBuild {
        SourceBuild {
            url: format!("https://codeload.github.com/example/app/tar.gz/{commit}"),
            commit: commit.to_owned(),
            sha256: sha(b"source"),
            cargo_lock: lock.to_owned(),
            toolchain: RustToolchainSpec::parse("rust:1.83.0").unwrap(),
            build: CargoBuildInvocation::parse("cargo build --locked --release").unwrap(),
            cache: NativeBuildCachePolicy::ContentHash,
        }
    }

    fn toolchain(home: &Path) -> RustToolchain {
        let root = home.join("toolchains/rust/1.83.0-aarch64-apple-darwin");
        RustToolchain {
            version: "1.83.0".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            cargo: root.join("cargo/bin/cargo"),
            cargo_home: root.join("cargo"),
            rustup_home: root.join("rustup"),
            bin_dir: root.join("cargo/bin"),
            root,
        }
    }

    /// A commit-shaped source archive: one top-level directory named after the
    /// commit, carrying a Cargo.lock.
    fn archive(root: &Path, commit: &str, lock: &str) -> PathBuf {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let prefix = format!("app-{commit}");
        for (name, bytes) in [
            (format!("{prefix}/Cargo.lock"), lock.as_bytes().to_vec()),
            (
                format!("{prefix}/Cargo.toml"),
                b"[package]\nname = \"app\"\n".to_vec(),
            ),
            (format!("{prefix}/src/main.rs"), b"fn main() {}\n".to_vec()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, name, std::io::Cursor::new(bytes))
                .unwrap();
        }
        let encoder = builder.into_inner().unwrap();
        let bytes = encoder.finish().unwrap();
        let path = root.join("source.tar.gz");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_build_is_cached_by_its_inputs_and_the_second_run_does_not_build() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let lock = sha(b"Cargo.lock bytes");
        let build = source_build(COMMIT, &lock);
        let source = archive(scratch.path(), COMMIT, "Cargo.lock bytes");
        let toolchain = toolchain(&home);

        let executor = FakeCargo::new();
        let first = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain,
            &source,
            &executor,
        )
        .unwrap();
        assert_eq!(first.cache, BuildCache::Miss);
        assert_eq!(executor.calls.get(), 1);
        assert_eq!(fs::read(&first.entry).unwrap(), b"built binary");

        let second = build_with(
            &build,
            &home,
            &scratch.path().join("second"),
            &toolchain,
            &source,
            &executor,
        )
        .unwrap();
        assert_eq!(second.cache, BuildCache::Hit);
        assert_eq!(executor.calls.get(), 1, "a cache hit must not build");
        assert_eq!(second.output_digest, first.output_digest);
        assert_eq!(second.cache_key, first.cache_key);
        assert_eq!(fs::read(&second.entry).unwrap(), b"built binary");
    }

    #[test]
    fn concurrent_builds_publish_one_complete_cache_entry() {
        struct ConcurrentCargo(std::sync::Barrier);
        impl BuildExecutor for ConcurrentCargo {
            fn run(&self, execution: &BuildExecution<'_>) -> Result<(), BuildFailure> {
                self.0.wait();
                fs::create_dir_all(execution.target_dir.join("release")).unwrap();
                fs::write(execution.target_dir.join("release/app"), b"built binary").unwrap();
                Ok(())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let source = archive(root.path(), COMMIT, "Cargo.lock bytes");
        let recipe = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let toolchain = toolchain(&home);
        let executor = ConcurrentCargo(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            let handles = ["a", "b"].map(|name| {
                let run = root.path().join(name);
                let (recipe, home, toolchain, source, executor) =
                    (&recipe, &home, &toolchain, &source, &executor);
                scope.spawn(move || {
                    build_with(recipe, home, &run, toolchain, source, executor).unwrap()
                })
            });
            let [a, b] = handles.map(|handle| handle.join().unwrap());
            assert_eq!(a.output_digest, b.output_digest);
            assert_eq!(fs::read(a.entry).unwrap(), fs::read(b.entry).unwrap());
        });
        let no_build = FakeCargo {
            calls: Cell::new(0),
            fail: true,
        };
        let cached = build_with(
            &recipe,
            &home,
            &root.path().join("cached"),
            &toolchain,
            &source,
            &no_build,
        )
        .unwrap();
        assert_eq!(cached.cache, BuildCache::Hit);
        assert_eq!(no_build.calls.get(), 0);
    }

    #[test]
    fn an_unusable_build_temp_directory_is_refused_before_cargo_starts() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let source = archive(scratch.path(), COMMIT, "Cargo.lock bytes");
        let working = scratch.path().join("native-build/scratch");
        fs::create_dir_all(&working).unwrap();
        fs::write(working.join("tmp"), b"not a directory").unwrap();
        let executor = FakeCargo::new();
        let error = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain(&home),
            &source,
            &executor,
        )
        .unwrap_err();
        assert!(matches!(error, BuildFailure::Io(_)), "{error}");
        assert_eq!(executor.calls.get(), 0);
        assert!(!home.join(BUILD_CACHE_DIR).exists());
    }

    #[test]
    fn every_input_that_decides_the_output_decides_the_cache_key() {
        let base = source_build(COMMIT, &sha(b"lock"));
        let key = cache_key(&base, "aarch64-apple-darwin", ENTRY);

        let mut commit = base.clone();
        commit.commit = "a".repeat(40);
        let mut source = base.clone();
        source.sha256 = sha(b"other source");
        let mut lock = base.clone();
        lock.cargo_lock = sha(b"other lock");
        let mut spec = base.clone();
        spec.toolchain = RustToolchainSpec::parse("rust:1.84.0").unwrap();
        let mut argv = base.clone();
        argv.build = CargoBuildInvocation::parse("cargo build --locked --release -p app").unwrap();

        for (what, candidate) in [
            ("commit", commit),
            ("source", source),
            ("lock", lock),
            ("toolchain", spec),
            ("argv", argv),
        ] {
            assert_ne!(
                cache_key(&candidate, "aarch64-apple-darwin", ENTRY),
                key,
                "{what}"
            );
        }
        assert_ne!(cache_key(&base, "x86_64-apple-darwin", ENTRY), key);
        assert_ne!(
            cache_key(
                &base,
                "aarch64-apple-darwin",
                ".ato-build/target/release/other"
            ),
            key
        );
        assert_eq!(cache_key(&base, "aarch64-apple-darwin", ENTRY), key);
    }

    #[test]
    fn a_tarball_for_another_commit_is_refused_before_building() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let source = archive(scratch.path(), &"0".repeat(40), "Cargo.lock bytes");
        let executor = FakeCargo::new();
        let error = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain(&home),
            &source,
            &executor,
        )
        .unwrap_err();
        assert!(
            matches!(error, BuildFailure::CommitMismatch { .. }),
            "{error}"
        );
        assert_eq!(executor.calls.get(), 0, "nothing may be built");
    }

    #[test]
    fn a_cargo_lock_that_is_not_the_pinned_one_is_refused_before_building() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"the pinned lock"));
        let source = archive(scratch.path(), COMMIT, "a different lock");
        let executor = FakeCargo::new();
        let error = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain(&home),
            &source,
            &executor,
        )
        .unwrap_err();
        assert!(
            matches!(error, BuildFailure::CargoLockMismatch { .. }),
            "{error}"
        );
        assert_eq!(executor.calls.get(), 0);
    }

    #[test]
    fn a_failed_build_reports_the_failure_and_writes_no_cache_entry() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let source = archive(scratch.path(), COMMIT, "Cargo.lock bytes");
        let executor = FakeCargo {
            calls: Cell::new(0),
            fail: true,
        };
        let error = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain(&home),
            &source,
            &executor,
        )
        .unwrap_err();
        assert!(matches!(error, BuildFailure::Failed(_)), "{error}");
        let key = cache_key(&build, "aarch64-apple-darwin", ENTRY);
        let hex = key.trim_start_matches("sha256:");
        assert!(!home.join(BUILD_CACHE_DIR).join(hex).exists());
    }

    #[test]
    fn a_cargo_that_produces_nothing_is_a_failure() {
        struct Silent;
        impl BuildExecutor for Silent {
            fn run(&self, _execution: &BuildExecution<'_>) -> Result<(), BuildFailure> {
                Ok(())
            }
        }
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let source = archive(scratch.path(), COMMIT, "Cargo.lock bytes");
        let error = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain(&home),
            &source,
            &Silent,
        )
        .unwrap_err();
        assert!(matches!(error, BuildFailure::OutputMissing(_)), "{error}");
    }

    #[test]
    fn a_cache_entry_whose_bytes_changed_is_discarded_rather_than_used() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let build = source_build(COMMIT, &sha(b"Cargo.lock bytes"));
        let source = archive(scratch.path(), COMMIT, "Cargo.lock bytes");
        let toolchain = toolchain(&home);
        let executor = FakeCargo::new();
        let first = build_with(
            &build,
            &home,
            scratch.path(),
            &toolchain,
            &source,
            &executor,
        )
        .unwrap();
        assert_eq!(first.cache, BuildCache::Miss);

        let key = cache_key(&build, "aarch64-apple-darwin", ENTRY);
        let cached = home
            .join(BUILD_CACHE_DIR)
            .join(key.trim_start_matches("sha256:"))
            .join(ENTRY);
        fs::write(&cached, b"tampered").unwrap();

        let second = build_with(
            &build,
            &home,
            &scratch.path().join("again"),
            &toolchain,
            &source,
            &executor,
        )
        .unwrap();
        assert_eq!(
            second.cache,
            BuildCache::Miss,
            "a tampered entry must not be trusted"
        );
        assert_eq!(executor.calls.get(), 2);
        assert_eq!(fs::read(&second.entry).unwrap(), b"built binary");
    }

    #[test]
    fn the_entry_must_name_one_file_under_the_staged_prefix() {
        assert_eq!(staged_file_name(ENTRY), Some("app"));
        for refused in [
            ".ato-build/target/release/",
            ".ato-build/target/release/a/b",
            "target/release/app",
            ".ato-build/target/debug/app",
            "app",
            "",
        ] {
            assert_eq!(staged_file_name(refused), None, "{refused}");
        }
        assert!(matches!(
            build(
                &source_build(COMMIT, &sha(b"lock")),
                "app",
                Path::new("/nonexistent"),
                Path::new("/nonexistent"),
                "aarch64-apple-darwin",
                &toolchain(Path::new("/nonexistent")),
                Path::new("/nonexistent/source.tar.gz"),
                &FakeCargo::new(),
            )
            .unwrap_err(),
            BuildFailure::EntryInvalid(_)
        ));
    }

    #[test]
    fn the_build_environment_is_child_scoped_and_carries_no_host_secret() {
        let home = tempfile::tempdir().unwrap();
        let toolchain = toolchain(home.path());
        let environment = build_environment(
            &toolchain,
            &home.path().join("target"),
            &home.path().join("scratch"),
        );
        assert_eq!(
            environment.get("CARGO_HOME").map(String::as_str),
            Some(toolchain.cargo_home.display().to_string().as_str())
        );
        assert_eq!(
            environment.get("RUSTUP_HOME").map(String::as_str),
            Some(toolchain.rustup_home.display().to_string().as_str())
        );
        assert_eq!(
            environment.get("CARGO_TARGET_DIR").map(String::as_str),
            Some(home.path().join("target").display().to_string().as_str())
        );
        assert!(environment.get("PATH").unwrap().contains("/usr/bin:/bin"));
        for inherited in ["AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN", "SSH_AUTH_SOCK"] {
            assert!(!environment.contains_key(inherited), "{inherited}");
        }
    }

    #[test]
    fn the_build_command_is_the_pinned_one_with_an_absolute_cargo() {
        let invocation =
            CargoBuildInvocation::parse("cargo build --locked --release -p app").unwrap();
        let command = build_command(&invocation, Path::new("/home/u/.ato/cargo/bin/cargo"));
        assert_eq!(
            command,
            vec![
                "/home/u/.ato/cargo/bin/cargo",
                "build",
                "--locked",
                "--release",
                "-p",
                "app",
            ]
        );
    }
}
