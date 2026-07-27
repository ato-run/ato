//! Building from a Source Revision that was already materialized, and from
//! nothing else.
//!
//! # What this exists to make impossible
//!
//! A v1 build must produce the tree the identity receipt commits to. The ways
//! that silently stops being true all look reasonable in isolation:
//!
//! - re-cloning the repository, because the archive fetch failed
//! - re-resolving a branch, because the commit "should" be the same
//! - reusing a checkout already on the host, because it is right there
//! - picking up the latest recipe, because that is what a fresh build would do
//! - falling back to Git when the archive is missing
//! - continuing past a tree-digest mismatch, because the build "mostly" matches
//!
//! Each substitutes source that was never verified for source that was, and
//! each does it at exactly the moment the verified path is unavailable — that
//! is, at the moment the substitution is least likely to be noticed.
//!
//! # How it is made impossible rather than merely forbidden
//!
//! [`ArchiveOnlyBuildInput`] carries four fields: a revision id, an archive
//! digest, an object key, and the tree digest to expect. There is no repository
//! URL, no owner, no repo name, no ref, no branch, and no host path — so a
//! clone or a branch resolution has nothing to clone or resolve *from*. The
//! forbidden operations are not rejected at runtime; they are unrepresentable in
//! the input type, which is why this module contains no Git-refusing branch and
//! needs none.
//!
//! Source arrives through [`SourceArchiveFetch`], whose only argument is an
//! object key. Its implementation cannot widen that: a key names bytes in a
//! content-addressed store, and there is no repository coordinate to hand it.
//!
//! The one runtime refusal that remains is the tree-digest check, because a
//! store *can* return the wrong bytes. That refusal is terminal.
//!
//! # Why the object key is re-derived
//!
//! The caller supplies the key, and this module recomputes it from the archive
//! digest and requires the two to agree. A key that pointed somewhere else would
//! otherwise be an unverified locator that the rest of the pipeline treats as
//! verified — and since the tree digest is checked against the fetched bytes, a
//! wrong key would be caught, but only after fetching. Deriving first turns a
//! network round trip into a comparison.

use std::fmt;
use std::path::{Path, PathBuf};

use capsule::contract::program_source_projection::{
    MaterializedProgramSource, VerifiedPinnedSourceMaterialization,
    materialize_program_source_projection,
};

use crate::source_materialization::{
    SourceMaterializationError, object_key_for_archive, verify_fetched_archive,
};

/// The archive as it lands on disk before verification.
const FETCHED_ARCHIVE_NAME: &str = "source.tar.zst";
/// Where the projected program source is materialized for the build.
const PROJECTION_DIR_NAME: &str = "projected-source";

/// Why an archive-only build refused to start.
///
/// Every variant is terminal. None has a "try the other way" arm, because the
/// other way is the one this module exists to remove.
#[derive(Debug)]
pub enum ArchiveOnlyBuildRefusal {
    /// A digest that is not `<algorithm>:<lowercase hex>`.
    MalformedDigest { field: &'static str, value: String },
    /// The revision id is empty or not a revision id.
    MalformedRevisionId { value: String },
    /// The declared key is not the one the archive digest implies.
    ObjectKeyNotContentAddressed { declared: String, derived: String },
    /// The store could not produce the object. NOT a reason to reach for Git.
    ArchiveUnavailable { object_key: String, reason: String },
    /// The fetched bytes are not the archive that was promised, or they are an
    /// archive of a different tree. Terminal in both cases.
    ArchiveDoesNotMatchItsIdentity { source: SourceMaterializationError },
    /// The archive is well-formed but its contents are not projectable source.
    ProjectionFailed { reason: String },
    /// The workspace this build was handed is unusable.
    WorkspaceUnusable { reason: String },
}

impl ArchiveOnlyBuildRefusal {
    /// A stable code for the API and for logs.
    ///
    /// Deliberately carries no digest body, path, or credential — the caller
    /// logs this alongside the revision id and the job id, and nothing else.
    pub fn code(&self) -> &'static str {
        match self {
            Self::MalformedDigest { .. } => "MALFORMED_DIGEST",
            Self::MalformedRevisionId { .. } => "MALFORMED_SOURCE_REVISION_ID",
            Self::ObjectKeyNotContentAddressed { .. } => "OBJECT_KEY_NOT_CONTENT_ADDRESSED",
            Self::ArchiveUnavailable { .. } => "SOURCE_ARCHIVE_UNAVAILABLE",
            Self::ArchiveDoesNotMatchItsIdentity { .. } => "SOURCE_ARCHIVE_IDENTITY_MISMATCH",
            Self::ProjectionFailed { .. } => "SOURCE_PROJECTION_FAILED",
            Self::WorkspaceUnusable { .. } => "BUILD_WORKSPACE_UNUSABLE",
        }
    }
}

impl fmt::Display for ArchiveOnlyBuildRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedDigest { field, value } => {
                write!(f, "{field} is not <algorithm>:<hex>: {value}")
            }
            Self::MalformedRevisionId { value } => {
                write!(f, "source revision id is malformed: {value}")
            }
            Self::ObjectKeyNotContentAddressed { declared, derived } => write!(
                f,
                "object key {declared} is not the content-addressed key for the \
                 archive digest, which is {derived}"
            ),
            Self::ArchiveUnavailable { object_key, reason } => write!(
                f,
                "the source archive at {object_key} could not be fetched ({reason}); \
                 an archive-only build has no other source and does not fall back"
            ),
            Self::ArchiveDoesNotMatchItsIdentity { source } => {
                write!(f, "the fetched archive is not the pinned source: {source}")
            }
            Self::ProjectionFailed { reason } => {
                write!(
                    f,
                    "the archive does not project to program source: {reason}"
                )
            }
            Self::WorkspaceUnusable { reason } => write!(f, "build workspace unusable: {reason}"),
        }
    }
}

impl std::error::Error for ArchiveOnlyBuildRefusal {}

/// Fetches an object from the content-addressed source-archive store.
///
/// The key is the only input. That is the point: an implementation has no
/// repository coordinate available to it, so no implementation of this trait can
/// turn a failed fetch into a clone.
pub trait SourceArchiveFetch {
    /// Write the object at `object_key` to `destination`.
    ///
    /// `Err` when the object could not be produced, for any reason. The caller
    /// treats every such failure as terminal.
    fn fetch(&self, object_key: &str, destination: &Path) -> Result<(), String>;
}

/// The complete input to an archive-only build.
///
/// Constructed only through [`ArchiveOnlyBuildInput::new`], so a value of this
/// type is proof that the object key is the one its archive digest implies.
/// Fields are private for that reason: a caller that could assemble one
/// field-by-field could assemble an inconsistent one.
#[derive(Debug, Clone)]
pub struct ArchiveOnlyBuildInput {
    source_revision_id: String,
    source_archive_digest: String,
    source_archive_object_key: String,
    expected_source_tree_digest: String,
}

impl ArchiveOnlyBuildInput {
    /// Validate the four pinned inputs, including that the key matches the digest.
    pub fn new(
        source_revision_id: impl Into<String>,
        source_archive_digest: impl Into<String>,
        source_archive_object_key: impl Into<String>,
        expected_source_tree_digest: impl Into<String>,
    ) -> Result<Self, ArchiveOnlyBuildRefusal> {
        let source_revision_id = source_revision_id.into();
        let source_archive_digest = source_archive_digest.into();
        let source_archive_object_key = source_archive_object_key.into();
        let expected_source_tree_digest = expected_source_tree_digest.into();

        if source_revision_id.is_empty()
            || !source_revision_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(ArchiveOnlyBuildRefusal::MalformedRevisionId {
                value: source_revision_id,
            });
        }
        require_digest_label("source_archive_digest", &source_archive_digest)?;
        require_digest_label("expected_source_tree_digest", &expected_source_tree_digest)?;

        // Re-derived, not trusted. A mismatch here costs a string comparison; the
        // same mismatch discovered after the fetch costs a network round trip and
        // leaves a wrong object on disk.
        let derived = object_key_for_archive(&source_archive_digest).map_err(|_| {
            ArchiveOnlyBuildRefusal::MalformedDigest {
                field: "source_archive_digest",
                value: source_archive_digest.clone(),
            }
        })?;
        if derived != source_archive_object_key {
            return Err(ArchiveOnlyBuildRefusal::ObjectKeyNotContentAddressed {
                declared: source_archive_object_key,
                derived,
            });
        }

        Ok(Self {
            source_revision_id,
            source_archive_digest,
            source_archive_object_key,
            expected_source_tree_digest,
        })
    }

    pub fn source_revision_id(&self) -> &str {
        &self.source_revision_id
    }

    pub fn source_archive_digest(&self) -> &str {
        &self.source_archive_digest
    }

    pub fn source_archive_object_key(&self) -> &str {
        &self.source_archive_object_key
    }

    pub fn expected_source_tree_digest(&self) -> &str {
        &self.expected_source_tree_digest
    }
}

fn require_digest_label(field: &'static str, value: &str) -> Result<(), ArchiveOnlyBuildRefusal> {
    let malformed = || ArchiveOnlyBuildRefusal::MalformedDigest {
        field,
        value: value.to_string(),
    };
    let (algorithm, hex) = value.split_once(':').ok_or_else(malformed)?;
    if algorithm.is_empty() || hex.is_empty() {
        return Err(malformed());
    }
    if !hex
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(malformed());
    }
    Ok(())
}

/// Program source, projected and ready to build, with the revision it came from.
///
/// Holding one of these is proof that the bytes on disk hash to the tree digest
/// the identity receipt commits to.
#[derive(Debug)]
pub struct AcquiredPinnedSource {
    source_revision_id: String,
    projection_root: PathBuf,
    materialized: MaterializedProgramSource,
}

impl AcquiredPinnedSource {
    /// The revision this source is, for propagation onto the build job.
    pub fn source_revision_id(&self) -> &str {
        &self.source_revision_id
    }

    /// The directory the build reads. Contains projected source and nothing else.
    pub fn projection_root(&self) -> &Path {
        &self.projection_root
    }

    pub fn materialized(&self) -> &MaterializedProgramSource {
        &self.materialized
    }
}

/// Fetch, verify, and project the pinned source for an archive-only build.
///
/// The order is fetch → verify → project, and the verify step is the whole
/// point: it re-derives both the archive digest and the tree digest from the
/// fetched bytes. A store that returns the wrong object, a truncated transfer,
/// and an archive of a different tree stored under a colliding key are three
/// different failures that this one check catches.
///
/// A tree-digest mismatch returns `Err` and leaves nothing projected. There is
/// deliberately no branch that continues with the fetched tree, and no branch
/// that reaches for Git — a build that proceeded on unverified source would
/// produce an artifact attributed to a Source Revision it does not contain.
pub fn acquire_pinned_source(
    input: &ArchiveOnlyBuildInput,
    fetch: &dyn SourceArchiveFetch,
    workdir: &Path,
) -> Result<AcquiredPinnedSource, ArchiveOnlyBuildRefusal> {
    std::fs::create_dir_all(workdir).map_err(|e| ArchiveOnlyBuildRefusal::WorkspaceUnusable {
        reason: format!("create the build workspace: {e}"),
    })?;

    let archive = workdir.join(FETCHED_ARCHIVE_NAME);
    fetch
        .fetch(input.source_archive_object_key(), &archive)
        .map_err(|reason| ArchiveOnlyBuildRefusal::ArchiveUnavailable {
            object_key: input.source_archive_object_key().to_string(),
            reason,
        })?;

    verify_fetched_archive(
        &archive,
        input.source_archive_digest(),
        input.expected_source_tree_digest(),
    )
    .map_err(|source| ArchiveOnlyBuildRefusal::ArchiveDoesNotMatchItsIdentity { source })?;

    // Only past the verification does anything get unpacked into a place the
    // build can read. `from_source_archive` extracts into its own private
    // directory and drops it on failure, so a rejected archive leaves nothing.
    let pinned =
        VerifiedPinnedSourceMaterialization::from_source_archive(&archive).map_err(|e| {
            ArchiveOnlyBuildRefusal::ProjectionFailed {
                reason: e.to_string(),
            }
        })?;

    let projection_root = workdir.join(PROJECTION_DIR_NAME);
    std::fs::create_dir_all(&projection_root).map_err(|e| {
        ArchiveOnlyBuildRefusal::WorkspaceUnusable {
            reason: format!("create the projection directory: {e}"),
        }
    })?;

    let materialized =
        materialize_program_source_projection(&pinned, &projection_root).map_err(|e| {
            ArchiveOnlyBuildRefusal::ProjectionFailed {
                reason: e.to_string(),
            }
        })?;

    Ok(AcquiredPinnedSource {
        source_revision_id: input.source_revision_id().to_string(),
        projection_root,
        materialized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use tempfile::TempDir;

    const REV: &str = "srev_000000000000000000000001";
    const ARCHIVE_DIGEST: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const TREE_DIGEST: &str =
        "blake3:2222222222222222222222222222222222222222222222222222222222222222";

    fn key_for(digest: &str) -> String {
        object_key_for_archive(digest).expect("well-formed digest")
    }

    fn input() -> ArchiveOnlyBuildInput {
        ArchiveOnlyBuildInput::new(REV, ARCHIVE_DIGEST, key_for(ARCHIVE_DIGEST), TREE_DIGEST)
            .expect("valid input")
    }

    /// Records what it was asked for, and writes whatever it was told to write.
    struct RecordingFetch {
        body: Option<Vec<u8>>,
        keys: RefCell<Vec<String>>,
    }

    impl RecordingFetch {
        fn serving(body: Vec<u8>) -> Self {
            Self {
                body: Some(body),
                keys: RefCell::new(Vec::new()),
            }
        }
        fn unavailable() -> Self {
            Self {
                body: None,
                keys: RefCell::new(Vec::new()),
            }
        }
    }

    impl SourceArchiveFetch for RecordingFetch {
        fn fetch(&self, object_key: &str, destination: &Path) -> Result<(), String> {
            self.keys.borrow_mut().push(object_key.to_string());
            match &self.body {
                Some(bytes) => {
                    fs::write(destination, bytes).map_err(|e| e.to_string())?;
                    Ok(())
                }
                None => Err("no such object".to_string()),
            }
        }
    }

    #[test]
    fn the_input_carries_no_repository_coordinate() {
        // A compile-time property, asserted here so the intent is recorded: the
        // struct's whole surface is these four accessors. If a repository URL,
        // ref, or host path is ever added, this test is where the reviewer is
        // meant to stop and ask what it is for.
        let i = input();
        assert_eq!(i.source_revision_id(), REV);
        assert_eq!(i.source_archive_digest(), ARCHIVE_DIGEST);
        assert_eq!(i.source_archive_object_key(), key_for(ARCHIVE_DIGEST));
        assert_eq!(i.expected_source_tree_digest(), TREE_DIGEST);
    }

    #[test]
    fn refuses_an_object_key_that_is_not_derived_from_the_digest() {
        let refusal = ArchiveOnlyBuildInput::new(
            REV,
            ARCHIVE_DIGEST,
            "source-archives/sha256/somewhere-else",
            TREE_DIGEST,
        )
        .expect_err("a key that is not content-addressed must be refused");
        assert_eq!(refusal.code(), "OBJECT_KEY_NOT_CONTENT_ADDRESSED");
    }

    #[test]
    fn refuses_a_key_for_a_different_archive() {
        // The shape is right, the content-addressing is right, but it addresses
        // a different archive. This is the substitution the check exists for.
        let other = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
        let refusal = ArchiveOnlyBuildInput::new(REV, ARCHIVE_DIGEST, key_for(other), TREE_DIGEST)
            .expect_err("a key for another archive must be refused");
        assert_eq!(refusal.code(), "OBJECT_KEY_NOT_CONTENT_ADDRESSED");
    }

    #[test]
    fn refuses_malformed_digests() {
        for (digest, tree) in [
            ("deadbeef", TREE_DIGEST),
            ("sha256:", TREE_DIGEST),
            (":abc", TREE_DIGEST),
            (ARCHIVE_DIGEST, "not-a-digest"),
            // Uppercase hex would be a second spelling of one digest, and two
            // spellings of an identity are two identities.
            (ARCHIVE_DIGEST, "blake3:AAAA"),
        ] {
            let refusal = ArchiveOnlyBuildInput::new(REV, digest, key_for(ARCHIVE_DIGEST), tree)
                .expect_err("malformed digest must be refused");
            assert_eq!(refusal.code(), "MALFORMED_DIGEST", "for {digest} / {tree}");
        }
    }

    #[test]
    fn refuses_a_malformed_revision_id() {
        for id in ["", "srev/../etc", "srev 001"] {
            let refusal = ArchiveOnlyBuildInput::new(
                id,
                ARCHIVE_DIGEST,
                key_for(ARCHIVE_DIGEST),
                TREE_DIGEST,
            )
            .expect_err("malformed revision id must be refused");
            assert_eq!(refusal.code(), "MALFORMED_SOURCE_REVISION_ID");
        }
    }

    #[test]
    fn fetches_exactly_the_declared_key_and_nothing_else() {
        let work = TempDir::new().expect("tempdir");
        let fetch = RecordingFetch::serving(b"not a real archive".to_vec());
        let _ = acquire_pinned_source(&input(), &fetch, work.path());
        assert_eq!(fetch.keys.borrow().as_slice(), &[key_for(ARCHIVE_DIGEST)]);
    }

    #[test]
    fn a_missing_archive_is_terminal_and_projects_nothing() {
        let work = TempDir::new().expect("tempdir");
        let fetch = RecordingFetch::unavailable();
        let refusal = acquire_pinned_source(&input(), &fetch, work.path())
            .expect_err("a missing archive must not be recoverable");
        assert_eq!(refusal.code(), "SOURCE_ARCHIVE_UNAVAILABLE");
        // The absence of a fallback is the property: nothing was projected, and
        // in particular no checkout was produced by another route.
        assert!(!work.path().join(PROJECTION_DIR_NAME).exists());
    }

    #[test]
    fn bytes_that_are_not_the_promised_archive_are_terminal() {
        let work = TempDir::new().expect("tempdir");
        let fetch = RecordingFetch::serving(b"wrong bytes".to_vec());
        let refusal = acquire_pinned_source(&input(), &fetch, work.path())
            .expect_err("an archive whose digest does not match must be refused");
        assert_eq!(refusal.code(), "SOURCE_ARCHIVE_IDENTITY_MISMATCH");
        assert!(!work.path().join(PROJECTION_DIR_NAME).exists());
    }

    /// A real source tree, frozen into a real archive.
    ///
    /// Built with `materialize_source_archive`, which takes a plain directory —
    /// so this whole fixture, and every test that uses it, runs without git.
    /// That is itself part of what is under test: an archive-only build has no
    /// git in its path, and a test that needed one would say otherwise.
    fn real_archive(dir: &Path) -> (PathBuf, String, String) {
        let checkout = dir.join("checkout");
        fs::create_dir_all(checkout.join("app")).expect("mkdir");
        fs::write(
            checkout.join("capsule.toml"),
            "[capsule]\nname = \"menuflow\"\n",
        )
        .expect("write manifest");
        fs::write(checkout.join("app").join("main.py"), "print('hello')\n").expect("write source");

        let archive = dir.join("fixture.tar.zst");
        let materialized =
            capsule::blob::materialize_source_archive(&checkout, &archive).expect("archive");
        (
            archive,
            materialized.source_archive_hash,
            materialized.materialized_source_tree_hash,
        )
    }

    #[test]
    fn a_verified_archive_projects_into_buildable_source() {
        let fixtures = TempDir::new().expect("tempdir");
        let (archive, archive_digest, tree_digest) = real_archive(fixtures.path());
        let bytes = fs::read(&archive).expect("read fixture");

        let work = TempDir::new().expect("tempdir");
        let acquired = acquire_pinned_source(
            &ArchiveOnlyBuildInput::new(
                REV,
                &archive_digest,
                key_for(&archive_digest),
                &tree_digest,
            )
            .expect("valid input"),
            &RecordingFetch::serving(bytes),
            work.path(),
        )
        .expect("a matching archive must project");

        assert_eq!(acquired.source_revision_id(), REV);
        assert!(
            acquired
                .projection_root()
                .join("app")
                .join("main.py")
                .is_file()
        );
        // The manifest is withheld from the projection by design: it is not what
        // the guest runs and not what the source digest names.
        assert!(!acquired.projection_root().join("capsule.toml").exists());
    }

    #[test]
    fn a_tree_digest_that_does_not_match_is_terminal() {
        // The archive is real and its BYTES verify — only the tree it contains is
        // not the one the identity receipt commits to. This is the case a build
        // would be most tempted to continue past, because everything about the
        // transfer succeeded.
        let fixtures = TempDir::new().expect("tempdir");
        let (archive, archive_digest, tree_digest) = real_archive(fixtures.path());
        let bytes = fs::read(&archive).expect("read fixture");

        let wrong_tree = format!("sha256:{}", "9".repeat(64));
        assert_ne!(wrong_tree, tree_digest);

        let work = TempDir::new().expect("tempdir");
        let refusal = acquire_pinned_source(
            &ArchiveOnlyBuildInput::new(
                REV,
                &archive_digest,
                key_for(&archive_digest),
                &wrong_tree,
            )
            .expect("valid input"),
            &RecordingFetch::serving(bytes),
            work.path(),
        )
        .expect_err("a tree-digest mismatch must not be survivable");

        assert_eq!(refusal.code(), "SOURCE_ARCHIVE_IDENTITY_MISMATCH");
        // Nothing was projected, so no build can read unverified source.
        assert!(!work.path().join(PROJECTION_DIR_NAME).exists());
    }

    /// The prohibition, enforced against this file's own text.
    ///
    /// The other tests show that today's code has no Git path. This one is aimed
    /// at the actual risk, which is not today: it is the plausible six-months-
    /// from-now change that adds "if the archive is missing, clone it" to fix a
    /// flaky fetch. Such a change would leave every other test in this file
    /// passing, because the fallback only fires where the tests expect a
    /// refusal — and a refusal is what a fallback replaces.
    ///
    /// A reviewer who genuinely needs one of these words here should be made to
    /// delete this test explicitly, in a diff someone reads.
    #[test]
    fn this_module_cannot_reach_git_or_a_repository() {
        let source = include_str!("archive_only_build.rs");
        // Everything below `mod tests` is fixture scaffolding, not the build path.
        let production = source
            .split_once("#[cfg(test)]")
            .expect("this file has a test module")
            .0;
        // Comments are prose — the module doc necessarily NAMES the operations it
        // forbids. Only code is scanned, so the guard cannot be tripped by an
        // accurate description of itself.
        let code: String = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "Command::new", // no subprocess at all, so no `git` by any name
            "git_checkout_pinned",
            "clone_pinned_source",
            "Repository::clone",
            "github.com",
            "checkout_source_tree",
            "materialize_source", // the git-side materializer, not ours
        ] {
            assert!(
                !code.contains(forbidden),
                "the archive-only build path must not reference `{forbidden}`: an \
                 archive-only build has exactly one source of source, and a second \
                 one added here would be reached precisely when the verified path \
                 is unavailable"
            );
        }

        // The input type is the other half: no repository coordinate to clone from.
        for coordinate in [
            "repository_url",
            "github_owner",
            "github_repo",
            "branch",
            "git_ref",
        ] {
            assert!(
                !code.contains(coordinate),
                "ArchiveOnlyBuildInput must not carry `{coordinate}` — the forbidden \
                 operations are meant to be unrepresentable, not merely unwritten"
            );
        }
    }

    #[test]
    fn refusal_messages_carry_no_receipt_body_or_credential() {
        let refusal = ArchiveOnlyBuildRefusal::ArchiveUnavailable {
            object_key: key_for(ARCHIVE_DIGEST),
            reason: "403".to_string(),
        };
        let rendered = refusal.to_string();
        assert!(rendered.contains("does not fall back"));
        assert!(!rendered.contains("http"));
        assert!(!rendered.contains("Authorization"));
    }
}
