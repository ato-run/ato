//! Bringing a pinned source archive down, and proving it is the right one
//! before anything can build from it.
//!
//! # The states are types
//!
//! ```text
//! DownloadedSourceArchive      bytes on disk, believed to be nothing
//!   → DigestVerifiedArchive    the BYTES are the ones the revision names
//!   → TreeVerifiedArchive      the CONTENTS are the tree the identity commits to
//!   → (v1 intake)
//! ```
//!
//! Each transition is the only way to reach the next state, and each consumes
//! the previous one. So a build function that takes a `TreeVerifiedArchive`
//! cannot be handed a path, a URL, or a downloaded-but-unchecked file — not
//! because a caller would remember not to, but because there is no value of that
//! type which has not been through both checks.
//!
//! This is the difference that matters. A `&Path` parameter with a doc comment
//! saying "must be verified first" is a convention; a type that can only be
//! constructed by verification is a guarantee.
//!
//! # Why both checks, and why neither is skippable
//!
//! The bytes can be wrong — a truncated transfer, the wrong object, tampering —
//! and the contents can be wrong while the bytes are intact, if an archive of a
//! different tree were stored under a colliding key. They fail independently, so
//! a consumer that checked only the archive digest would accept an archive whose
//! tree is not the one the identity receipt commits to.
//!
//! # There is no other way to get source
//!
//! A failed download is terminal. Not a reason to clone the repository, not a
//! reason to reuse a local materialization directory from an earlier job, and
//! not a reason to fall back to a recipe. Each of those substitutes source that
//! was never verified for source that was, at exactly the moment the verified
//! path is unavailable — which is when the substitution is least likely to be
//! noticed.

use std::fmt;
use std::path::{Path, PathBuf};

use snapshot::archive_only_build::ArchiveOnlyBuildInput;
use snapshot::source_materialization::verify_fetched_archive;

/// How many times to attempt the transfer before giving up.
///
/// Bounded for the same reason the upload is: an unbounded retry against a
/// failing store is how one job consumes a builder indefinitely.
pub const MAX_DOWNLOAD_ATTEMPTS: u32 = 3;

/// Why the pinned source could not be obtained.
///
/// Every variant is terminal. None has a "try the repository instead" arm.
#[derive(Debug)]
pub enum DownloadFailure {
    /// The API would not authorize a read. Carries its code, never a URL.
    AuthorizationRefused { code: String, detail: String },
    /// Every attempt failed. Carries the last status, never a URL.
    TransferFailed { attempts: u32, detail: String },
    /// The bytes are not the archive the revision names.
    ArchiveDigestMismatch { detail: String },
    /// The bytes are intact but contain a different tree.
    TreeDigestMismatch { detail: String },
    /// The workspace could not be prepared.
    WorkspaceUnusable { detail: String },
}

impl DownloadFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AuthorizationRefused { .. } => "download_authorization_refused",
            Self::TransferFailed { .. } => "download_transfer_failed",
            Self::ArchiveDigestMismatch { .. } => "source_archive_digest_mismatch",
            Self::TreeDigestMismatch { .. } => "source_tree_digest_mismatch",
            Self::WorkspaceUnusable { .. } => "download_workspace_unusable",
        }
    }
}

impl fmt::Display for DownloadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorizationRefused { code, detail } => write!(
                f,
                "the API refused to authorize the download ({code}): {detail}; \
                 an archive-only build has no other source of source"
            ),
            Self::TransferFailed { attempts, detail } => write!(
                f,
                "the archive did not transfer after {attempts} attempts: {detail}; \
                 this build does not fall back to the repository"
            ),
            Self::ArchiveDigestMismatch { detail } => {
                write!(f, "the fetched bytes are not the pinned archive: {detail}")
            }
            Self::TreeDigestMismatch { detail } => write!(
                f,
                "the archive is intact but contains a different tree: {detail}"
            ),
            Self::WorkspaceUnusable { detail } => {
                write!(f, "the download workspace is unusable: {detail}")
            }
        }
    }
}

/// Asks the API to authorize one read, and performs it.
///
/// A seam, so the retry and refusal behaviour is testable without a store —
/// those are the paths a live test exercises least.
pub trait ArchiveDownloadTransport {
    /// Ask the API for a short-lived read grant on this job's archive.
    ///
    /// Returns the URL. The caller uses it immediately and keeps nothing.
    fn authorize(&self, job_id: &str) -> Result<String, DownloadFailure>;

    /// GET `url` into `destination`. Returns the number of bytes written.
    fn get(&self, url: &str, destination: &Path) -> Result<u64, String>;
}

/// Bytes on disk. Believed to be nothing yet.
///
/// The path is private: the whole point of the chain is that an unverified path
/// cannot be handed to anything, and a public field would let it.
#[derive(Debug)]
pub struct DownloadedSourceArchive {
    path: PathBuf,
}

/// The bytes are the ones the pinned revision names.
///
/// Constructible only by [`DownloadedSourceArchive::verify_digests`], which also
/// checks the tree — so this state exists to name the step, not to be a stopping
/// point a caller can rest at.
#[derive(Debug)]
pub struct DigestVerifiedArchive {
    path: PathBuf,
}

/// The contents are the tree the identity receipt commits to.
///
/// The only type the build path accepts. Holding one is proof that both the
/// bytes and what they unpack to were checked against the pinned revision.
#[derive(Debug)]
pub struct TreeVerifiedArchive {
    path: PathBuf,
}

impl TreeVerifiedArchive {
    /// The archive, for extraction. Reachable only from a verified value.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl DownloadedSourceArchive {
    /// Step one: are these the BYTES the pinned revision names?
    ///
    /// Consumes the downloaded value, so the unverified state is gone from the
    /// caller's hands rather than merely unused by it.
    pub fn verify_archive_digest(
        self,
        input: &ArchiveOnlyBuildInput,
    ) -> Result<DigestVerifiedArchive, DownloadFailure> {
        let bytes = std::fs::read(&self.path).map_err(|e| DownloadFailure::WorkspaceUnusable {
            detail: format!("read the fetched archive: {e}"),
        })?;
        let actual = capsule::blob::source_archive_hash(&bytes);
        if actual != input.source_archive_digest() {
            return Err(DownloadFailure::ArchiveDigestMismatch {
                detail: format!(
                    "expected {}, fetched {actual}",
                    input.source_archive_digest()
                ),
            });
        }
        Ok(DigestVerifiedArchive { path: self.path })
    }
}

impl DigestVerifiedArchive {
    /// Step two: do those bytes CONTAIN the tree the identity commits to?
    ///
    /// A separate step because the two fail independently. Intact bytes of a
    /// different tree pass step one and must not pass step two — an archive of
    /// something else stored under a colliding key would otherwise be built.
    ///
    /// Delegates to `verify_fetched_archive`, which re-derives both digests
    /// (extracting and re-hashing for the tree). The byte check therefore runs
    /// twice; that is deliberate, so the final state is guaranteed by the one
    /// shared function rather than by this module having remembered both halves.
    pub fn verify_tree_digest(
        self,
        input: &ArchiveOnlyBuildInput,
    ) -> Result<TreeVerifiedArchive, DownloadFailure> {
        verify_fetched_archive(
            &self.path,
            input.source_archive_digest(),
            input.expected_source_tree_digest(),
        )
        .map_err(|e| DownloadFailure::TreeDigestMismatch {
            detail: e.to_string(),
        })?;
        Ok(TreeVerifiedArchive { path: self.path })
    }
}

/// Fetch the pinned archive and prove it, or fail terminally.
///
/// Returns the only type the build path will accept.
pub fn download_pinned_source(
    transport: &dyn ArchiveDownloadTransport,
    job_id: &str,
    input: &ArchiveOnlyBuildInput,
    workdir: &Path,
) -> Result<TreeVerifiedArchive, DownloadFailure> {
    std::fs::create_dir_all(workdir).map_err(|e| DownloadFailure::WorkspaceUnusable {
        detail: format!("{e}"),
    })?;
    let destination = workdir.join("source.tar.zst");

    let mut last = String::new();
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        // A fresh grant each time, for the same reason the upload asks again:
        // holding a URL across retries turns a short TTL into a long one.
        let url = transport.authorize(job_id)?;
        match transport.get(&url, &destination) {
            Ok(_) => {
                // The chain, in full. Each step consumes the previous state, so
                // there is no way to reach the build with a half-checked value.
                return DownloadedSourceArchive { path: destination }
                    .verify_archive_digest(input)?
                    .verify_tree_digest(input);
            }
            Err(e) => {
                // No URL in the message: failure details are acked and stored.
                last = format!("attempt {attempt}: {e}");
            }
        }
    }

    Err(DownloadFailure::TransferFailed {
        attempts: MAX_DOWNLOAD_ATTEMPTS,
        detail: last,
    })
}

/// The real transport: ato-api for the grant, the store for the bytes.
pub struct HttpArchiveDownloadTransport<'a> {
    pub api_url: &'a str,
    pub token: &'a str,
    pub agent_id: &'a str,
}

impl ArchiveDownloadTransport for HttpArchiveDownloadTransport<'_> {
    fn authorize(&self, job_id: &str) -> Result<String, DownloadFailure> {
        let response = ureq::post(&format!(
            "{}/v1/capsule-snapshots/jobs/{job_id}/source-archive/download-authorization",
            self.api_url
        ))
        .set("authorization", &format!("Bearer {}", self.token))
        .send_json(ureq::json!({ "agent_id": self.agent_id }));

        let body: serde_json::Value = match response {
            Ok(r) => r
                .into_json()
                .map_err(|e| DownloadFailure::AuthorizationRefused {
                    code: "unreadable_response".into(),
                    detail: format!("{e}"),
                })?,
            Err(ureq::Error::Status(status, r)) => {
                let text = r.into_string().unwrap_or_default();
                let code = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(String::from)))
                    .unwrap_or_else(|| format!("http_{status}"));
                return Err(DownloadFailure::AuthorizationRefused {
                    code,
                    detail: format!("authorization returned HTTP {status}"),
                });
            }
            Err(e) => {
                return Err(DownloadFailure::AuthorizationRefused {
                    code: "transport".into(),
                    detail: format!("{e}"),
                });
            }
        };

        body.get("download_url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| DownloadFailure::AuthorizationRefused {
                code: "malformed_authorization".into(),
                detail: "response has no download_url".into(),
            })
    }

    fn get(&self, url: &str, destination: &Path) -> Result<u64, String> {
        // `ureq`'s transport-error Display can include the URL, which is a bearer
        // credential — only the kind is reported.
        let response = ureq::get(url).call().map_err(|e| match e {
            ureq::Error::Status(status, _) => format!("HTTP {status}"),
            ureq::Error::Transport(t) => format!("transport error: {}", t.kind()),
        })?;
        let mut reader = response.into_reader();
        let mut file = std::fs::File::create(destination).map_err(|e| e.to_string())?;
        std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tempfile::TempDir;

    const ARCHIVE: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const TREE: &str = "blake3:2222222222222222222222222222222222222222222222222222222222222222";
    const REV: &str = "srev_000000000000000000000001";

    fn key_for(digest: &str) -> String {
        snapshot::source_materialization::object_key_for_archive(digest).expect("key")
    }

    fn input() -> ArchiveOnlyBuildInput {
        ArchiveOnlyBuildInput::new(REV, ARCHIVE, key_for(ARCHIVE), TREE).expect("valid")
    }

    struct FakeTransport {
        results: RefCell<Vec<Result<u64, String>>>,
        body: Vec<u8>,
        authorize_calls: RefCell<u32>,
        urls: RefCell<Vec<String>>,
        refuse: bool,
    }

    impl FakeTransport {
        fn serving(body: Vec<u8>, results: Vec<Result<u64, String>>) -> Self {
            Self {
                results: RefCell::new(results),
                body,
                authorize_calls: RefCell::new(0),
                urls: RefCell::new(Vec::new()),
                refuse: false,
            }
        }
        fn refusing() -> Self {
            Self {
                results: RefCell::new(Vec::new()),
                body: Vec::new(),
                authorize_calls: RefCell::new(0),
                urls: RefCell::new(Vec::new()),
                refuse: true,
            }
        }
    }

    impl ArchiveDownloadTransport for FakeTransport {
        fn authorize(&self, _job_id: &str) -> Result<String, DownloadFailure> {
            if self.refuse {
                return Err(DownloadFailure::AuthorizationRefused {
                    code: "SOURCE_ARCHIVE_TRANSPORT_UNCONFIGURED".into(),
                    detail: "not configured".into(),
                });
            }
            let n = {
                let mut c = self.authorize_calls.borrow_mut();
                *c += 1;
                *c
            };
            let url = format!("https://store.example/obj?X-Amz-Signature=sig{n}");
            self.urls.borrow_mut().push(url.clone());
            Ok(url)
        }

        fn get(&self, _url: &str, destination: &Path) -> Result<u64, String> {
            let outcome = {
                let mut r = self.results.borrow_mut();
                if r.is_empty() {
                    Ok(self.body.len() as u64)
                } else {
                    r.remove(0)
                }
            };
            match outcome {
                Ok(n) => {
                    std::fs::write(destination, &self.body).map_err(|e| e.to_string())?;
                    Ok(n)
                }
                Err(e) => Err(e),
            }
        }
    }

    #[test]
    fn bytes_that_are_not_the_pinned_archive_do_not_become_verified() {
        let work = TempDir::new().expect("tempdir");
        let transport = FakeTransport::serving(b"wrong bytes".to_vec(), vec![]);
        let failure = download_pinned_source(&transport, "job_1", &input(), work.path())
            .expect_err("unverified bytes must not pass");
        assert_eq!(failure.code(), "source_archive_digest_mismatch");
    }

    #[test]
    fn asks_for_a_fresh_grant_on_every_retry() {
        let work = TempDir::new().expect("tempdir");
        let transport = FakeTransport::serving(
            b"x".to_vec(),
            vec![Err("reset".into()), Err("timeout".into()), Ok(1)],
        );
        // The third attempt succeeds at transfer and then fails verification,
        // which is fine — what is under test is the grant behaviour.
        let _ = download_pinned_source(&transport, "job_1", &input(), work.path());
        assert_eq!(*transport.authorize_calls.borrow(), 3);
        let urls = transport.urls.borrow();
        assert_ne!(urls[0], urls[1]);
        assert_ne!(urls[1], urls[2]);
    }

    #[test]
    fn gives_up_after_a_bounded_number_of_attempts() {
        let work = TempDir::new().expect("tempdir");
        let transport = FakeTransport::serving(
            b"x".to_vec(),
            vec![
                Err("a".into()),
                Err("b".into()),
                Err("c".into()),
                Err("d".into()),
            ],
        );
        let failure = download_pinned_source(&transport, "job_1", &input(), work.path())
            .expect_err("must give up");
        assert_eq!(failure.code(), "download_transfer_failed");
        assert_eq!(*transport.authorize_calls.borrow(), MAX_DOWNLOAD_ATTEMPTS);
    }

    #[test]
    fn an_authorization_refusal_stops_immediately() {
        let work = TempDir::new().expect("tempdir");
        let transport = FakeTransport::refusing();
        let failure = download_pinned_source(&transport, "job_1", &input(), work.path())
            .expect_err("must refuse");
        assert_eq!(failure.code(), "download_authorization_refused");
    }

    #[test]
    fn no_failure_message_carries_a_url() {
        // Failure details are acked to ato-api and stored. A presigned URL in one
        // is a bearer credential in a database.
        let work = TempDir::new().expect("tempdir");
        let transport = FakeTransport::serving(
            b"x".to_vec(),
            vec![Err("a".into()), Err("b".into()), Err("c".into())],
        );
        let failure =
            download_pinned_source(&transport, "job_1", &input(), work.path()).expect_err("fail");
        let rendered = failure.to_string();
        assert!(!rendered.contains("X-Amz-Signature"));
        assert!(!rendered.contains("https://store.example"));
    }

    #[test]
    fn every_failure_says_there_is_no_other_source() {
        // The message is the last line of defence against someone "fixing" a
        // download failure by adding a clone.
        for failure in [
            DownloadFailure::AuthorizationRefused {
                code: "x".into(),
                detail: "y".into(),
            },
            DownloadFailure::TransferFailed {
                attempts: 3,
                detail: "z".into(),
            },
        ] {
            let rendered = failure.to_string();
            assert!(
                rendered.contains("no other source") || rendered.contains("does not fall back"),
                "{rendered}"
            );
        }
    }

    /// The structural claim, asserted against this file's own text.
    ///
    /// The other tests show today's code has no repository path. This one is
    /// aimed at the change six months from now that adds "if the download fails,
    /// clone it" to fix a flaky store — a change that would leave every other
    /// test here passing, because a fallback only fires where they expect a
    /// failure.
    #[test]
    fn this_module_cannot_reach_a_repository() {
        let source = include_str!("source_archive_download.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("has a test module")
            .0;
        let code: String = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "Command::new",
            "git_checkout_pinned",
            "clone_pinned_source",
            "checkout_source_tree",
            "github.com",
            "materialize_source",
        ] {
            assert!(
                !code.contains(forbidden),
                "the download path must not reference `{forbidden}`: a build with no \
                 verified archive has no source, and a second way to obtain one would \
                 be reached exactly when the verified path is unavailable"
            );
        }
    }
}
