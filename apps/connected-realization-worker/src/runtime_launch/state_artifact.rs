//! Transport and on-disk form for an `ato.state.filesystem@1` attachment.
//!
//! ## Why this is new code
//!
//! Nothing existing carries a filesystem tree as a content-addressed artifact.
//! `lib/objects` addresses a *computation* graph, the builder's only output
//! producer emits a web-serving bundle, and nacelle's tar helpers unpack a
//! self-extracting executable. Each is the wrong shape, so a state tree gets
//! its own packing.
//!
//! What is NOT new is the identity and the transport. The digest is a
//! `sha256:` content address re-verified on arrival, and bytes reach the
//! Runner over the same lease-scoped authenticated request the object graph
//! already uses — the Runner holds no R2 binding and learns no bucket name.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

/// The state protocol this module implements. A Runner refuses any other.
pub const STATE_ARTIFACT_FORMAT: &str = "ato.state.filesystem@1";

/// The largest state artifact this Runner will pack or accept.
///
/// Enforced on BOTH sides. The control plane's limit protects the control
/// plane; this one protects the Runner, which unpacks attacker-influenced
/// bytes onto its own disk and would otherwise fill it on behalf of one
/// tenant. Matches the control plane's 64 MiB so neither side is the surprise.
pub const MAX_STATE_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

/// The largest WORKSPACE this Runner will materialize.
///
/// Larger than a state artifact, and for a different reason. State is what a
/// tenant's users typed; a workspace is a build output that legitimately
/// carries an interpreter and an installed dependency tree — the Python
/// acceptance fixture is 112 MB with a stripped CPython and FastAPI in it. One
/// limit for both would either refuse ordinary workspaces or let state grow to
/// a size nothing intends.
pub const MAX_WORKSPACE_ARTIFACT_BYTES: usize = 1024 * 1024 * 1024;

/// A packed state tree together with the content address of its bytes.
pub struct StateArtifact {
    digest: String,
    bytes: Vec<u8>,
}

impl StateArtifact {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl std::fmt::Debug for StateArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The digest is an identity and safe to log; the bytes are the user's
        // data and never are.
        formatter
            .debug_struct("StateArtifact")
            .field("digest", &self.digest)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

pub fn state_artifact_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Pack a state directory into a deterministic archive.
///
/// Deterministic on purpose: the digest is the state revision's identity, so
/// packing the same tree twice must produce the same address. Entry order is
/// sorted, and mtime, uid, gid and username are normalized away — none of them
/// are state, and leaving them in would mint a new revision on every commit.
/// The mode is reduced to one bit, whether the owner may execute, which is the
/// only part a restored tree needs to honour.
pub fn pack_state_tree(root: &Path) -> Result<StateArtifact> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_entries(root, root, &mut files, &mut directories)?;

    let mut bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut bytes);
        // Directories are recorded, including empty ones. `filesystem@1` names
        // a filesystem tree, and an empty directory is part of one: an app
        // that creates `uploads/` on first start and finds it gone after a
        // wake has lost state, even though no file was ever in it.
        for relative in &directories {
            let mut header = tar::Header::new_ustar();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Directory);
            let name = format!(
                "{}/",
                relative
                    .to_str()
                    .context("state directory name is not valid UTF-8")?
            );
            builder
                .append_data(&mut header, &name, std::io::empty())
                .with_context(|| format!("failed to pack state directory {name}"))?;
        }
        for relative in &files {
            let absolute = root.join(relative);
            let contents = std::fs::read(&absolute)
                .with_context(|| format!("failed to read state file {}", relative.display()))?;
            let executable = is_owner_executable(&absolute)?;

            let mut header = tar::Header::new_ustar();
            header.set_size(contents.len() as u64);
            header.set_mode(if executable { 0o755 } else { 0o644 });
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            let name = relative
                .to_str()
                .context("state file name is not valid UTF-8")?;
            builder
                .append_data(&mut header, name, Cursor::new(&contents))
                .with_context(|| format!("failed to pack state file {name}"))?;
        }
        builder
            .finish()
            .context("failed to finish the state archive")?;
    }

    Ok(StateArtifact {
        digest: state_artifact_digest(&bytes),
        bytes,
    })
}

/// Verify a downloaded artifact and expand it into `destination`, atomically.
///
/// Fail closed at every step, in this order:
///
/// 1. the byte count is checked, before anything is parsed;
/// 2. the digest is verified, before a single byte is written;
/// 3. every entry path is checked, before it is opened;
/// 4. the tree is built in a SIBLING directory and only then moved into place.
///
/// Step 4 is what makes it atomic. Unpacking straight into `destination` and
/// failing halfway would leave a partial tree that looks like state — an app
/// would open a truncated SQLite file and, at best, fail confusingly. On any
/// error the staging directory is removed and `destination` is untouched.
pub fn unpack_state_tree(bytes: &[u8], expected_digest: &str, destination: &Path) -> Result<()> {
    unpack_tree_bounded(
        bytes,
        expected_digest,
        destination,
        MAX_STATE_ARTIFACT_BYTES,
    )
}

/// The same expansion, under a workspace-sized bound.
pub fn unpack_workspace_tree(
    bytes: &[u8],
    expected_digest: &str,
    destination: &Path,
) -> Result<()> {
    unpack_tree_bounded(
        bytes,
        expected_digest,
        destination,
        MAX_WORKSPACE_ARTIFACT_BYTES,
    )
}

fn unpack_tree_bounded(
    bytes: &[u8],
    expected_digest: &str,
    destination: &Path,
    limit: usize,
) -> Result<()> {
    ensure!(
        bytes.len() <= limit,
        "artifact is {} bytes, over the {limit} byte limit",
        bytes.len()
    );

    let actual = state_artifact_digest(bytes);
    if actual != expected_digest {
        // Deliberately does not name the destination or echo the bytes: a
        // mismatch is exactly the case where the content is not trusted.
        bail!("state artifact digest mismatch: expected {expected_digest}, computed {actual}");
    }

    let parent = destination
        .parent()
        .context("state destination has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".ato-state-staging-")
        .tempdir_in(parent)
        .context("failed to create the state staging directory")?;

    // Errors propagate out of this block, and `staging` is removed on drop —
    // so a failure anywhere leaves `destination` exactly as it was.
    expand_into(bytes, staging.path(), limit)?;

    if destination.exists() {
        let discarded = tempfile::Builder::new()
            .prefix(".ato-state-replaced-")
            .tempdir_in(parent)
            .context("failed to stage the replaced state directory")?;
        let discarded_path = discarded.path().join("previous");
        std::fs::rename(destination, &discarded_path)
            .with_context(|| format!("failed to move aside {}", destination.display()))?;
        // The swap. If THIS fails the old tree is put back, because a
        // destination that exists neither as the old state nor as the new one
        // is the one outcome with no recovery.
        if let Err(error) = std::fs::rename(staging.path(), destination) {
            let _ = std::fs::rename(&discarded_path, destination);
            return Err(error).context("failed to swap in the materialized state");
        }
    } else {
        std::fs::rename(staging.path(), destination)
            .context("failed to move the materialized state into place")?;
    }
    // The rename consumed the staging directory; keep its guard from trying to
    // remove a path that is now the live state.
    let _ = staging.keep();
    Ok(())
}

fn expand_into(bytes: &[u8], staging: &Path, limit: usize) -> Result<()> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    // The archive is content-addressed but NOT trusted: its digest proves only
    // that it is the artifact the control plane named, not that whoever
    // produced it was well-behaved.
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut expanded: u64 = 0;
    for entry in archive.entries().context("state archive is unreadable")? {
        let mut entry = entry.context("state archive entry is unreadable")?;
        let entry_type = entry.header().entry_type();
        ensure!(
            entry_type.is_file() || entry_type.is_dir(),
            "state archive carries a non-regular entry, which this format does not define"
        );
        let path = entry.path().context("state archive entry has no path")?;
        let relative = safe_relative_path(path.as_ref())?;
        let target = staging.join(&relative);

        if entry_type.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", relative.display()))?;
            continue;
        }

        // Bound the EXPANDED size too. A compressed-format archive could
        // otherwise be small on the wire and unbounded on disk; this format is
        // uncompressed today, and the check keeps that from being load-bearing.
        expanded = expanded.saturating_add(entry.header().size().unwrap_or(0));
        ensure!(
            expanded <= limit as u64,
            "archive expands past the {limit} byte limit"
        );

        if let Some(directory) = target.parent() {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("failed to create {}", directory.display()))?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("failed to write state file {}", relative.display()))?;
    }
    Ok(())
}

/// Accept only a plain relative path. Absolute paths, `..`, drive prefixes and
/// root components are all refused rather than normalized, because every one
/// of them is a request to write outside the attachment.
fn safe_relative_path(path: &Path) -> Result<PathBuf> {
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!(
                    "state archive entry {} escapes the attachment root",
                    path.display()
                );
            }
        }
    }
    ensure!(
        !safe.as_os_str().is_empty(),
        "state archive entry has an empty path"
    );
    Ok(safe)
}

fn collect_entries(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?;
    for entry in entries {
        let entry = entry.context("failed to read a state directory entry")?;
        let path = entry.path();
        // `symlink_metadata` on purpose: a symlink is NOT followed. Following
        // one would let a link inside the state directory pull an arbitrary
        // host file into the artifact.
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("failed to stat {}", path.display()))?;
        if metadata.is_symlink() {
            bail!(
                "state directory contains a symlink ({}), which this format does not define",
                path.display()
            );
        }
        if metadata.is_dir() {
            directories.insert(
                path.strip_prefix(root)
                    .context("state directory escaped the state root")?
                    .to_path_buf(),
            );
            collect_entries(root, &path, files, directories)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .context("state file escaped the state root")?
                .to_path_buf();
            files.insert(relative);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_owner_executable(path: &Path) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let metadata =
        std::fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    Ok(metadata.permissions().mode() & 0o100 != 0)
}

#[cfg(not(unix))]
fn is_owner_executable(_path: &Path) -> Result<bool> {
    Ok(false)
}

// ------------------------------------------------------------------ transport

/// What the control plane returns when a Runner claims the writer for a state
/// slot: the revision to start from, and the fence that orders the writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWriterGrant {
    /// The revision the working copy must be materialized from, or `None` for
    /// a slot that has never been written.
    pub revision_ref: Option<String>,
    /// The content address of that revision's artifact. `None` iff
    /// `revision_ref` is `None`.
    pub artifact_digest: Option<String>,
    /// Monotonic, NOT a capability. It orders writers so a stale one cannot
    /// commit; it authorizes nothing, and an authenticated Runner holding its
    /// assigned Run is what authorizes the write.
    pub writer_fence: u64,
}

/// Moving state artifacts between the Runner and the control plane.
///
/// A trait so the executor can be exercised without a network, and so the
/// Runner has exactly one place that talks about state bytes.
pub trait StateArtifactTransport {
    /// Claim the writer for `state_key` on this Run's ComputeInstance.
    fn acquire_writer(&self, state_key: &str) -> Result<StateWriterGrant>;

    /// Download the artifact named by a content address. Implementations must
    /// return the raw bytes; verification belongs to the caller, which does it
    /// in `unpack_state_tree`.
    fn download(&self, artifact_digest: &str) -> Result<Vec<u8>>;

    /// Commit a new revision. `commit_request_id` makes the call idempotent so
    /// a retry after an ambiguous failure cannot fork the history.
    fn commit(
        &self,
        state_key: &str,
        writer_fence: u64,
        parent_revision_ref: Option<&str>,
        commit_request_id: &str,
        artifact: &StateArtifact,
    ) -> Result<String>;

    /// Give the slot back after a Run finished with it.
    ///
    /// Distinct from `abort_writer` only in intent, and both must be safe to
    /// call: a slot left held by a Run that is gone is a slot no future Run can
    /// take, which turns one failure into a permanently stuck App. Releasing
    /// twice, or releasing a slot someone else has since taken, is a no-op
    /// rather than an error.
    fn release_writer(&self, state_key: &str, writer_fence: u64) -> Result<()>;

    /// Give the slot back after a Run FAILED with it.
    ///
    /// Separate from `release_writer` so the control plane can tell the two
    /// apart in its own records; the fencing effect is identical. Default
    /// implementation defers to release, because getting the slot back matters
    /// more than distinguishing why.
    fn abort_writer(&self, state_key: &str, writer_fence: u64) -> Result<()> {
        self.release_writer(state_key, writer_fence)
    }
}

/// The real transport: lease-scoped, bearer-authenticated requests to the
/// control plane.
///
/// Modelled on the object-graph source the Runner already uses. The Runner
/// never holds an R2 binding and never learns a bucket name or an object key —
/// it names content by digest and the control plane resolves it.
pub struct LeaseStateArtifactTransport {
    client: reqwest::blocking::Client,
    base: String,
    lease_id: String,
    token: String,
}

impl LeaseStateArtifactTransport {
    pub fn new(
        client: reqwest::blocking::Client,
        base: impl Into<String>,
        lease_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            client,
            base: base.into(),
            lease_id: lease_id.into(),
            token: token.into(),
        }
    }

    fn url(&self, suffix: &str) -> String {
        format!(
            "{}/v1/runner-leases/{}/state/{}",
            self.base, self.lease_id, suffix
        )
    }
}

impl StateArtifactTransport for LeaseStateArtifactTransport {
    fn acquire_writer(&self, state_key: &str) -> Result<StateWriterGrant> {
        let response = self
            .client
            .post(self.url("writers"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "state_key": state_key }))
            .send()?
            .error_for_status()
            .context("failed to acquire the state writer")?;
        let body: serde_json::Value = response.json().context("writer grant is malformed")?;
        let writer_fence = body
            .get("writer_fence")
            .and_then(serde_json::Value::as_u64)
            .context("writer grant carries no fence")?;
        let revision_ref = body
            .get("revision_ref")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let artifact_digest = body
            .get("artifact_digest")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        ensure!(
            revision_ref.is_some() == artifact_digest.is_some(),
            "writer grant names a revision without its artifact, or the reverse"
        );
        Ok(StateWriterGrant {
            revision_ref,
            artifact_digest,
            writer_fence,
        })
    }

    fn download(&self, artifact_digest: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(self.url(&format!("artifacts/{artifact_digest}")))
            .bearer_auth(&self.token)
            .send()?
            .error_for_status()
            .context("failed to download the state artifact")?;
        Ok(response.bytes()?.to_vec())
    }

    fn release_writer(&self, state_key: &str, writer_fence: u64) -> Result<()> {
        self.client
            .post(self.url("writers/release"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "state_key": state_key,
                "writer_fence": writer_fence,
                "outcome": "released",
            }))
            .send()?
            .error_for_status()
            .context("failed to release the state writer")?;
        Ok(())
    }

    fn abort_writer(&self, state_key: &str, writer_fence: u64) -> Result<()> {
        self.client
            .post(self.url("writers/release"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "state_key": state_key,
                "writer_fence": writer_fence,
                "outcome": "aborted",
            }))
            .send()?
            .error_for_status()
            .context("failed to abort the state writer")?;
        Ok(())
    }

    fn commit(
        &self,
        state_key: &str,
        writer_fence: u64,
        parent_revision_ref: Option<&str>,
        commit_request_id: &str,
        artifact: &StateArtifact,
    ) -> Result<String> {
        let response = self
            .client
            .post(self.url("revisions"))
            .bearer_auth(&self.token)
            .header("content-type", "application/octet-stream")
            .header("x-ato-state-key", state_key)
            .header("x-ato-writer-fence", writer_fence.to_string())
            .header("x-ato-commit-request-id", commit_request_id)
            .header("x-ato-artifact-digest", artifact.digest())
            .header(
                "x-ato-parent-revision-ref",
                parent_revision_ref.unwrap_or(""),
            )
            .body(artifact.bytes().to_vec())
            .send()?
            .error_for_status()
            .context("failed to commit the state revision")?;
        let body: serde_json::Value = response.json().context("commit receipt is malformed")?;
        body.get("revision_ref")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .context("commit receipt names no revision")
    }
}

/// Materialize the working copy a launch will mount, from whatever the grant
/// says the slot currently holds.
pub fn materialize_working_copy(
    transport: &dyn StateArtifactTransport,
    grant: &StateWriterGrant,
    destination: &Path,
) -> Result<()> {
    let Some(digest) = grant.artifact_digest.as_deref() else {
        // A slot that has never been written materializes as an empty
        // directory, not as an absent mount: the app must find its state path
        // present and usable on the very first Run.
        std::fs::create_dir_all(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        return Ok(());
    };
    let bytes = transport.download(digest)?;
    unpack_state_tree(&bytes, digest, destination)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "app.db", "sqlite-bytes");
        write(dir.path(), "nested/notes.txt", "hello");
        dir
    }

    #[test]
    fn a_state_tree_round_trips_through_its_content_address() {
        let source = fixture();
        let artifact = pack_state_tree(source.path()).expect("packs");
        let restored = tempfile::tempdir().expect("tempdir");
        unpack_state_tree(artifact.bytes(), artifact.digest(), restored.path()).expect("unpacks");

        assert_eq!(
            std::fs::read_to_string(restored.path().join("app.db")).expect("read"),
            "sqlite-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(restored.path().join("nested/notes.txt")).expect("read"),
            "hello"
        );
        // Round-tripping is stable: repacking the restored tree lands on the
        // same address, which is what makes the digest a revision identity.
        assert_eq!(
            pack_state_tree(restored.path()).expect("repacks").digest(),
            artifact.digest()
        );
    }

    #[test]
    fn packing_ignores_everything_that_is_not_state() {
        let first = fixture();
        let second = fixture();
        // Written at different moments, and (on a busy machine) by different
        // runs. If mtime or ownership reached the archive, these would differ
        // and every commit would mint a spurious revision.
        std::fs::File::options()
            .write(true)
            .open(second.path().join("app.db"))
            .expect("open")
            .set_modified(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
            )
            .expect("mtime");
        assert_eq!(
            pack_state_tree(first.path()).expect("packs").digest(),
            pack_state_tree(second.path()).expect("packs").digest()
        );
    }

    #[test]
    fn a_digest_mismatch_writes_nothing() {
        let source = fixture();
        let artifact = pack_state_tree(source.path()).expect("packs");
        let destination = tempfile::tempdir().expect("tempdir");
        let wrong = format!("sha256:{}", "0".repeat(64));

        let error = unpack_state_tree(artifact.bytes(), &wrong, destination.path()).unwrap_err();
        assert!(error.to_string().contains("digest mismatch"));
        // Fail closed: the check runs before any byte is written.
        assert!(
            std::fs::read_dir(destination.path())
                .expect("read_dir")
                .next()
                .is_none()
        );
    }

    fn archive_with_entry(name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let contents = b"owned";
            let mut header = tar::Header::new_ustar();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            // The `tar` crate refuses to WRITE a `..` path, so a hostile
            // archive has to be forged by hand — which is exactly the archive
            // an attacker would send, and the reason the reader must check.
            let raw = header.as_old_mut();
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder
                .append(&header, Cursor::new(&contents[..]))
                .expect("append");
            builder.finish().expect("finish");
        }
        bytes
    }

    #[test]
    fn an_entry_that_escapes_the_attachment_is_refused() {
        for name in ["../escape", "nested/../../escape"] {
            let bytes = archive_with_entry(name);
            let digest = state_artifact_digest(&bytes);
            let destination = tempfile::tempdir().expect("tempdir");
            let error = unpack_state_tree(&bytes, &digest, destination.path()).unwrap_err();
            assert!(
                error.to_string().contains("escapes the attachment root"),
                "{name} was not refused: {error}"
            );
        }
    }

    #[test]
    fn a_symlink_in_the_state_directory_is_refused_rather_than_followed() {
        // Following one would let a link inside the attachment pull an
        // arbitrary host file into a user-visible artifact.
        let source = fixture();
        let secret = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(secret.path(), "host-secret").expect("write");
        std::os::unix::fs::symlink(secret.path(), source.path().join("link")).expect("symlink");

        let error = pack_state_tree(source.path()).unwrap_err();
        assert!(error.to_string().contains("symlink"));
    }

    #[test]
    fn an_unwritten_slot_materializes_as_an_empty_directory() {
        struct NeverCalled;
        impl StateArtifactTransport for NeverCalled {
            fn acquire_writer(&self, _key: &str) -> Result<StateWriterGrant> {
                unreachable!()
            }
            fn download(&self, _digest: &str) -> Result<Vec<u8>> {
                panic!("an unwritten slot must not download anything")
            }
            fn commit(
                &self,
                _key: &str,
                _fence: u64,
                _parent: Option<&str>,
                _request: &str,
                _artifact: &StateArtifact,
            ) -> Result<String> {
                unreachable!()
            }
            fn release_writer(&self, _key: &str, _fence: u64) -> Result<()> {
                Ok(())
            }
        }

        let destination = tempfile::tempdir().expect("tempdir");
        let target = destination.path().join("working");
        materialize_working_copy(
            &NeverCalled,
            &StateWriterGrant {
                revision_ref: None,
                artifact_digest: None,
                writer_fence: 1,
            },
            &target,
        )
        .expect("materializes");
        // Present and usable on the very first Run, not absent.
        assert!(target.is_dir());
    }

    #[test]
    fn the_artifact_never_prints_its_bytes() {
        let source = fixture();
        let rendered = format!("{:?}", pack_state_tree(source.path()).expect("packs"));
        assert!(!rendered.contains("sqlite-bytes"));
        assert!(rendered.contains("sha256:"));
    }

    #[test]
    fn an_empty_directory_survives_a_round_trip() {
        // `filesystem@1` names a filesystem TREE. An app that creates
        // `uploads/` on first start and finds it gone after a wake has lost
        // state, even though no file was ever in it.
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(source.path().join("uploads")).expect("mkdir");
        std::fs::create_dir_all(source.path().join("nested/deep")).expect("mkdir");
        let artifact = pack_state_tree(source.path()).expect("packs");

        let restored = tempfile::tempdir().expect("tempdir");
        let target = restored.path().join("state");
        unpack_state_tree(artifact.bytes(), artifact.digest(), &target).expect("unpacks");
        assert!(target.join("uploads").is_dir());
        assert!(target.join("nested/deep").is_dir());
        assert_eq!(
            pack_state_tree(&target).expect("repacks").digest(),
            artifact.digest()
        );
    }

    #[test]
    fn a_failed_unpack_leaves_the_previous_state_intact() {
        // The dangerous outcome is not "the restore failed" — it is a
        // destination holding half a tree that looks like state, where an app
        // opens a truncated database and reports success.
        let previous = tempfile::tempdir().expect("tempdir");
        let live = previous.path().join("state");
        std::fs::create_dir_all(&live).expect("mkdir");
        std::fs::write(live.join("app.sqlite"), b"the-real-state").expect("write");

        let hostile = archive_with_entry("../escape");
        let digest = state_artifact_digest(&hostile);
        unpack_state_tree(&hostile, &digest, &live).unwrap_err();

        assert_eq!(
            std::fs::read(live.join("app.sqlite")).expect("read"),
            b"the-real-state"
        );
        // And no staging leftovers beside it.
        let strays: Vec<_> = std::fs::read_dir(previous.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ato-state")
            })
            .collect();
        assert!(strays.is_empty(), "staging directories were left behind");
    }

    #[test]
    fn an_oversized_artifact_is_refused_before_it_is_written() {
        let destination = tempfile::tempdir().expect("tempdir");
        let target = destination.path().join("state");
        let oversized = vec![0u8; MAX_STATE_ARTIFACT_BYTES + 1];
        let error =
            unpack_state_tree(&oversized, &state_artifact_digest(&oversized), &target).unwrap_err();
        assert!(error.to_string().contains("over the"), "{error}");
        assert!(!target.exists());
    }
}
