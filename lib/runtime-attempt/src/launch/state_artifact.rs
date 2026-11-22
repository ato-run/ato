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
//!
//! ## Encoding is transport; the canonical tar is the identity
//!
//! A revision's content address is the sha256 of the UNCOMPRESSED canonical
//! tar, exactly as before gzip existed. The Runner gzips that tar on the wire
//! (`x-ato-artifact-encoding: gzip`) and expands whatever encoding the control
//! plane answers with, so a revision stored by an older Runner as the plain tar
//! still restores. Neither the tar nor the encoded artifact is held in memory:
//! pack streams the tar through a gzip encoder into a temporary file while
//! hashing it, download streams the response into a temporary file, and
//! expansion streams through the decoder into the existing safe, bounded
//! extractor.

use std::collections::BTreeSet;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

/// The state protocol this module implements. A Runner refuses any other.
pub const STATE_ARTIFACT_FORMAT: &str = "ato.state.filesystem@1";

/// What this Runner announces to the control plane: it sends state artifacts
/// gzip encoded and accepts them the same way. Encoding never changes the
/// revision identity, so this is a transport capability, not a format change.
pub const STATE_ARTIFACT_GZIP_CAPABILITY: &str = "state_artifact_encoding=gzip";

/// The environment variable bounding the ENCODED bytes of one state artifact
/// this Runner sends or receives.
pub const STATE_ARTIFACT_MAX_BYTES_ENV: &str = "ATO_STATE_ARTIFACT_MAX_BYTES";

/// The environment variable bounding the UNCOMPRESSED canonical tar this
/// Runner will expand. It protects the Runner's disk, not the control plane.
pub const STATE_TREE_MAX_BYTES_ENV: &str = "ATO_STATE_TREE_MAX_BYTES";

/// Default for [`STATE_ARTIFACT_MAX_BYTES_ENV`]: the size limit this Runner had
/// while artifacts were uncompressed, kept so both sides agree by default.
pub const DEFAULT_STATE_ARTIFACT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Default for [`STATE_TREE_MAX_BYTES_ENV`]. Larger than the encoded default
/// because the tree is what is expanded onto the Runner's disk and state is
/// mostly compressible text: 64 MiB on the wire is routinely a much larger
/// tree.
pub const DEFAULT_STATE_TREE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The largest WORKSPACE this Runner will materialize.
///
/// Larger than a state artifact, and for a different reason. State is what a
/// tenant's users typed; a workspace is a build output that legitimately
/// carries an interpreter and an installed dependency tree — the Python
/// acceptance fixture is 112 MB with a stripped CPython and FastAPI in it. One
/// limit for both would either refuse ordinary workspaces or let state grow to
/// a size nothing intends. Workspaces stay in the identity (uncompressed tar)
/// form and are not part of the state artifact encoding change.
pub const MAX_WORKSPACE_ARTIFACT_BYTES: usize = 1024 * 1024 * 1024;

/// The two limits one state artifact transfer is subject to, read from this
/// Runner's environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateArtifactLimits {
    encoded_bytes: u64,
    tree_bytes: u64,
}

impl StateArtifactLimits {
    /// Read the limits from the environment.
    ///
    /// An unset, non-numeric or zero value falls back to its default: a silent
    /// zero would refuse every checkpoint, which is never what an administrator
    /// meant to configure.
    pub fn from_env() -> Self {
        Self::from_raw(
            std::env::var(STATE_ARTIFACT_MAX_BYTES_ENV).ok().as_deref(),
            std::env::var(STATE_TREE_MAX_BYTES_ENV).ok().as_deref(),
        )
    }

    fn from_raw(encoded: Option<&str>, tree: Option<&str>) -> Self {
        Self {
            encoded_bytes: parse_limit(encoded, DEFAULT_STATE_ARTIFACT_MAX_BYTES),
            tree_bytes: parse_limit(tree, DEFAULT_STATE_TREE_MAX_BYTES),
        }
    }

    /// The encoded bytes this Runner may send or receive.
    pub fn encoded_bytes(&self) -> u64 {
        self.encoded_bytes
    }

    /// The uncompressed tar bytes this Runner may expand.
    pub fn tree_bytes(&self) -> u64 {
        self.tree_bytes
    }
}

fn parse_limit(raw: Option<&str>, default: u64) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn ensure_encoded_within(len: u64, limits: &StateArtifactLimits) -> Result<()> {
    ensure!(
        len <= limits.encoded_bytes,
        "state artifact is {len} bytes encoded, over the {} byte limit named by \
         {STATE_ARTIFACT_MAX_BYTES_ENV}",
        limits.encoded_bytes
    );
    Ok(())
}

fn ensure_tree_within(len: u64, limits: &StateArtifactLimits) -> Result<()> {
    ensure!(
        len <= limits.tree_bytes,
        "state tree is {len} bytes uncompressed, over the {} byte limit named by \
         {STATE_TREE_MAX_BYTES_ENV}",
        limits.tree_bytes
    );
    Ok(())
}

/// How a state artifact's bytes are encoded on the wire.
///
/// Encoding is transport, not identity: `x-ato-artifact-digest` always names
/// the digest of the UNCOMPRESSED canonical tar, so the same tree has the same
/// content address whichever encoding carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactEncoding {
    /// The canonical tar itself. What a Runner sent before gzip existed, and
    /// what a portable bundle embeds.
    Identity,
    /// gzip of the canonical tar.
    Gzip,
}

impl ArtifactEncoding {
    /// The value of the `x-ato-artifact-encoding` header.
    pub const fn header_value(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Gzip => "gzip",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "identity" => Ok(Self::Identity),
            "gzip" => Ok(Self::Gzip),
            other => {
                bail!("state artifact is {other}-encoded, which this Runner does not understand")
            }
        }
    }
}

/// A packed state tree ready to upload.
///
/// The gzip-encoded bytes live in a temporary file, so a tree far larger than
/// this process should hold can still be committed; the digest is the content
/// address of the UNCOMPRESSED canonical tar the file decodes to.
pub struct StateArtifact {
    digest: String,
    file: tempfile::NamedTempFile,
    encoded_len: u64,
}

impl StateArtifact {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The encoding `commit` sends this artifact with.
    pub fn encoding(&self) -> ArtifactEncoding {
        ArtifactEncoding::Gzip
    }

    /// The number of bytes that will cross the wire.
    pub fn encoded_len(&self) -> u64 {
        self.encoded_len
    }

    /// The encoded bytes. Reading them materializes the artifact in memory,
    /// which the transport does not: `commit` streams the file.
    pub fn encoded_bytes(&self) -> Result<Vec<u8>> {
        std::fs::read(self.file.path()).with_context(|| {
            format!(
                "failed to read the packed state artifact {}",
                self.file.path().display()
            )
        })
    }

    fn reader(&self) -> Result<std::fs::File> {
        self.file
            .reopen()
            .context("failed to reopen the packed state artifact")
    }
}

impl std::fmt::Debug for StateArtifact {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The digest is an identity and safe to log; the bytes are the user's
        // data and never are.
        formatter
            .debug_struct("StateArtifact")
            .field("digest", &self.digest)
            .field("encoding", &self.encoding())
            .field("encoded_len", &format_args!("{} bytes", self.encoded_len))
            .finish()
    }
}

/// A writer that hashes and measures everything written through it.
///
/// The tar is hashed as it is written and BEFORE the gzip encoder sees it: the
/// digest is the identity of the uncompressed canonical tar, not of the
/// encoded artifact.
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }

    fn finish(self) -> (String, u64, W) {
        let Self {
            inner,
            hasher,
            written,
        } = self;
        (format!("sha256:{:x}", hasher.finalize()), written, inner)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn state_artifact_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Pack a state directory into the artifact `commit` uploads.
///
/// The canonical tar is written through a gzip encoder into a temporary file
/// and hashed as it is produced, so neither the tar nor the encoded artifact is
/// held in memory. The canonical tar itself is unchanged — same entry order,
/// same normalized headers — so a tree packed here has exactly the digest it
/// always had.
pub fn pack_state_tree(root: &Path) -> Result<StateArtifact> {
    pack_state_tree_with_limits(root, &StateArtifactLimits::from_env())
}

fn pack_state_tree_with_limits(root: &Path, limits: &StateArtifactLimits) -> Result<StateArtifact> {
    let parent = root
        .parent()
        .context("state directory has no parent directory")?;
    let file = tempfile::Builder::new()
        .prefix(".ato-state-artifact-")
        .tempfile_in(parent)
        .context("failed to create the state artifact file")?;
    let mut sink = HashingWriter::new(GzEncoder::new(file, Compression::default()));
    append_canonical_tar(root, &mut sink)?;
    let (digest, tree_bytes, encoder) = sink.finish();
    let mut file = encoder
        .finish()
        .context("failed to finish the compressed state artifact")?;
    file.as_file_mut()
        .flush()
        .context("failed to flush the state artifact file")?;
    let encoded_len = file
        .as_file()
        .metadata()
        .context("failed to stat the state artifact file")?
        .len();
    ensure_tree_within(tree_bytes, limits)?;
    ensure_encoded_within(encoded_len, limits)?;
    Ok(StateArtifact {
        digest,
        file,
        encoded_len,
    })
}

/// The canonical, UNCOMPRESSED tar of a state tree, in memory.
///
/// This is the identity form of the tree: the digest of these bytes is the
/// revision's content address, and it is what a portable bundle embeds. The
/// Runner's state transport does not use it — `pack_state_tree` streams the
/// same bytes through gzip — but the workspace artifact format and the bundle
/// object format are this tar unchanged.
pub fn pack_canonical_state_tree(root: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    append_canonical_tar(root, &mut bytes)?;
    Ok(bytes)
}

/// Write the deterministic canonical tar of `root` to `writer`.
///
/// Deterministic on purpose: the digest is the state revision's identity, so
/// packing the same tree twice must produce the same address. Entry order is
/// sorted, and mtime, uid, gid and username are normalized away — none of them
/// are state, and leaving them in would mint a new revision on every commit.
/// The mode is reduced to one bit, whether the owner may execute, which is the
/// only part a restored tree needs to honour.
fn append_canonical_tar<W: Write>(root: &Path, writer: &mut W) -> Result<()> {
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    collect_entries(root, root, &mut files, &mut directories)?;

    let mut builder = tar::Builder::new(writer);
    // Directories are recorded, including empty ones. `filesystem@1` names a
    // filesystem tree, and an empty directory is part of one: an app that
    // creates `uploads/` on first start and finds it gone after a wake has
    // lost state, even though no file was ever in it.
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
        let contents = std::fs::File::open(&absolute)
            .with_context(|| format!("failed to read state file {}", relative.display()))?;
        let size = contents
            .metadata()
            .with_context(|| format!("failed to stat state file {}", relative.display()))?
            .len();
        let executable = is_owner_executable(&absolute)?;

        let mut header = tar::Header::new_ustar();
        header.set_size(size);
        header.set_mode(if executable { 0o755 } else { 0o644 });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        let name = relative
            .to_str()
            .context("state file name is not valid UTF-8")?;
        builder
            .append_data(&mut header, name, contents)
            .with_context(|| format!("failed to pack state file {name}"))?;
    }
    builder
        .finish()
        .context("failed to finish the state archive")?;
    Ok(())
}

/// A downloaded state artifact whose encoded bytes live in a temporary file.
///
/// Verification and expansion stream through that file; the tree is never held
/// in memory, and the digest of the UNCOMPRESSED tar is checked before a single
/// byte is written to the destination.
pub struct DownloadedStateArtifact {
    file: tempfile::NamedTempFile,
    encoding: ArtifactEncoding,
}

impl DownloadedStateArtifact {
    /// Wrap bytes that were already read.
    ///
    /// A test seam, and the shape an identity artifact can be handed over in;
    /// the real transport streams the response into a temporary file instead.
    pub fn from_encoded_bytes(encoding: ArtifactEncoding, bytes: Vec<u8>) -> Result<Self> {
        let mut file = tempfile::Builder::new()
            .prefix(".ato-state-download-")
            .tempfile()
            .context("failed to create the downloaded state artifact file")?;
        file.as_file_mut()
            .write_all(&bytes)
            .context("failed to write the downloaded state artifact")?;
        file.as_file_mut()
            .flush()
            .context("failed to flush the downloaded state artifact")?;
        Ok(Self { file, encoding })
    }

    /// The encoding the control plane answered with.
    pub fn encoding(&self) -> ArtifactEncoding {
        self.encoding
    }

    /// Verify this artifact's digest and expand it into `destination`.
    ///
    /// Fail closed at every step, in this order:
    ///
    /// 1. the encoded byte count is checked, before anything is parsed;
    /// 2. the digest of the UNCOMPRESSED tar is verified, before a single byte
    ///    is written;
    /// 3. every entry path is checked, before it is opened;
    /// 4. the tree is built in a SIBLING directory and only then moved into
    ///    place.
    ///
    /// Step 4 is what makes it atomic. Unpacking straight into `destination`
    /// and failing halfway would leave a partial tree that looks like state —
    /// an app would open a truncated SQLite file and, at best, fail
    /// confusingly. On any error the staging directory is removed and
    /// `destination` is untouched.
    pub fn unpack(self, expected_digest: &str, destination: &Path) -> Result<()> {
        self.unpack_with(
            expected_digest,
            destination,
            &StateArtifactLimits::from_env(),
        )
    }

    fn unpack_with(
        self,
        expected_digest: &str,
        destination: &Path,
        limits: &StateArtifactLimits,
    ) -> Result<()> {
        let encoded_len = self
            .file
            .as_file()
            .metadata()
            .context("failed to stat the downloaded state artifact")?
            .len();
        let artifact = EncodedArtifact {
            source: ArtifactSource::File(self.file.path()),
            encoding: self.encoding,
            encoded_len,
        };
        unpack_state_artifact(&artifact, expected_digest, destination, limits)
    }
}

/// One artifact's encoded bytes, wherever they currently live.
struct EncodedArtifact<'a> {
    source: ArtifactSource<'a>,
    encoding: ArtifactEncoding,
    encoded_len: u64,
}

enum ArtifactSource<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

impl<'a> EncodedArtifact<'a> {
    fn reader(&self) -> Result<Box<dyn Read + 'a>> {
        Ok(match self.source {
            ArtifactSource::File(path) => decode(
                std::fs::File::open(path).with_context(|| {
                    format!("failed to open the state artifact {}", path.display())
                })?,
                self.encoding,
            ),
            ArtifactSource::Bytes(bytes) => decode(Cursor::new(bytes), self.encoding),
        })
    }
}

fn decode<'a>(reader: impl Read + 'a, encoding: ArtifactEncoding) -> Box<dyn Read + 'a> {
    match encoding {
        ArtifactEncoding::Identity => Box::new(reader),
        ArtifactEncoding::Gzip => Box::new(GzDecoder::new(reader)),
    }
}

/// Verify an in-memory artifact and expand it into `destination`, atomically.
///
/// This is the identity (uncompressed tar) form. A downloaded artifact is
/// expanded through its temporary file by [`DownloadedStateArtifact::unpack`],
/// which does not hold it in memory.
pub fn unpack_state_tree(bytes: &[u8], expected_digest: &str, destination: &Path) -> Result<()> {
    let artifact = EncodedArtifact {
        source: ArtifactSource::Bytes(bytes),
        encoding: ArtifactEncoding::Identity,
        encoded_len: bytes.len() as u64,
    };
    unpack_state_artifact(
        &artifact,
        expected_digest,
        destination,
        &StateArtifactLimits::from_env(),
    )
}

/// The same expansion, under a workspace-sized bound.
///
/// Workspaces stay in the identity form: they are immutable build outputs
/// published by the control plane, and their format did not gain an encoding.
pub fn unpack_workspace_tree(
    bytes: &[u8],
    expected_digest: &str,
    destination: &Path,
) -> Result<()> {
    let limit = MAX_WORKSPACE_ARTIFACT_BYTES as u64;
    ensure!(
        bytes.len() as u64 <= limit,
        "artifact is {} bytes, over the {limit} byte limit",
        bytes.len()
    );

    let actual = state_artifact_digest(bytes);
    if actual != expected_digest {
        // Deliberately does not name the destination or echo the bytes: a
        // mismatch is exactly the case where the content is not trusted.
        bail!("state artifact digest mismatch: expected {expected_digest}, computed {actual}");
    }

    materialize_tree(destination, TreePurpose::Workspace, |staging| {
        expand_into(
            Cursor::new(bytes),
            staging,
            limit,
            "MAX_WORKSPACE_ARTIFACT_BYTES",
        )
    })
}

fn unpack_state_artifact(
    artifact: &EncodedArtifact<'_>,
    expected_digest: &str,
    destination: &Path,
    limits: &StateArtifactLimits,
) -> Result<()> {
    // 1. The encoded size is bounded before anything is parsed.
    ensure_encoded_within(artifact.encoded_len, limits)?;

    // 2. The digest of the UNCOMPRESSED tar is checked before a single byte is
    //    written; the uncompressed size is counted on the same stream.
    let actual = decoded_digest(artifact, limits)?;
    if actual != expected_digest {
        // Deliberately does not name the destination or echo the bytes: a
        // mismatch is exactly the case where the content is not trusted.
        bail!("state artifact digest mismatch: expected {expected_digest}, computed {actual}");
    }

    // 3. Expand into a sibling and swap atomically.
    materialize_tree(destination, TreePurpose::State, |staging| {
        let mut reader = artifact.reader()?;
        expand_into(
            &mut reader,
            staging,
            limits.tree_bytes,
            STATE_TREE_MAX_BYTES_ENV,
        )
    })
}

/// The digest of the UNCOMPRESSED bytes an artifact decodes to.
///
/// This is the revision identity, so it is computed over the decoded stream
/// whatever the encoding was; the tree limit is counted here too, before the
/// decoded bytes reach the disk.
fn decoded_digest(artifact: &EncodedArtifact<'_>, limits: &StateArtifactLimits) -> Result<String> {
    let mut reader = artifact.reader()?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("state artifact is unreadable")?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        ensure_tree_within(total, limits)?;
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

enum TreePurpose {
    State,
    Workspace,
}

/// Build a tree in a sibling directory and swap it into `destination`.
///
/// The staging directory is on the destination's filesystem, so the swap is a
/// rename. Errors anywhere inside `expand` leave `destination` exactly as it
/// was: the staging directory is removed on drop.
fn materialize_tree(
    destination: &Path,
    purpose: TreePurpose,
    expand: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let parent = destination
        .parent()
        .context("state destination has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    if matches!(purpose, TreePurpose::Workspace) {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let staging = builder
        .prefix(".ato-state-staging-")
        .tempdir_in(parent)
        .context("failed to create the state staging directory")?;

    // Errors propagate out of this call, and `staging` is removed on drop —
    // so a failure anywhere leaves `destination` exactly as it was.
    expand(staging.path())?;
    #[cfg(unix)]
    if matches!(purpose, TreePurpose::Workspace) {
        prepare_readonly_workspace(staging.path())?;
    }

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

/// The immutable workspace is bind-mounted read-only for the workload, whose
/// UID may differ from the Runner's. Normalize only the tree being published.
#[cfg(unix)]
fn prepare_readonly_workspace(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut directories = vec![root.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            let mode = if kind.is_dir() {
                directories.push(path.clone());
                0o755
            } else {
                ensure!(kind.is_file(), "workspace contains a non-regular file");
                if is_owner_executable(&path)? {
                    0o755
                } else {
                    0o644
                }
            };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        }
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn expand_into(reader: impl Read, staging: &Path, limit: u64, limit_name: &str) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
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

        // Bound the EXPANDED size too. The artifact may be gzip encoded on
        // the wire, so it can be small there and unbounded on disk; the same
        // limit is counted on the decoded stream before extraction as well.
        expanded = expanded.saturating_add(entry.header().size().unwrap_or(0));
        ensure!(
            expanded <= limit,
            "state tree expands past the {limit} byte limit named by {limit_name}"
        );

        if let Some(directory) = target.parent() {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("failed to create {}", directory.display()))?;
        }
        // The mode is read BEFORE unpacking, because `unpack` consumes the
        // entry.
        #[cfg(unix)]
        let executable = entry.header().mode().unwrap_or(0o644) & 0o100 != 0;
        entry
            .unpack(&target)
            .with_context(|| format!("failed to write state file {}", relative.display()))?;
        // `set_preserve_permissions(false)` above is right — an archive must
        // not dictate host permissions — but it also drops the one bit the
        // packer deliberately recorded. Restoring just that bit is the
        // difference between a workspace whose interpreter runs and one whose
        // launch fails with a bare exit code, which is exactly how this was
        // found.
        #[cfg(unix)]
        if executable {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("failed to mark {} executable", relative.display()))?;
        }
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
    /// Present when the slot is backed by a Runner-local volume: which one,
    /// and what the control plane believes about it. For such a slot
    /// `revision_ref` is the one-time seed, and only while provisioning.
    pub volume: Option<GrantedVolume>,
}

/// The control plane's view of a Runner-local volume at grant time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedVolume {
    pub volume_ref: String,
    pub status: super::volume::VolumeStatus,
}

/// What a Runner tells the control plane about a volume it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeReport {
    /// Created (or found already created) on this Runner.
    Ready { volume_ref: String },
    /// Should be here and is not. The control plane stops scheduling it.
    Missing { volume_ref: String, reason: String },
    /// Bytes the volume occupies, measured after a confirmed stop.
    Usage {
        volume_ref: String,
        usage_bytes: u64,
    },
}

/// Moving state artifacts between the Runner and the control plane.
///
/// A trait so the executor can be exercised without a network, and so the
/// Runner has exactly one place that talks about state bytes.
pub trait StateArtifactTransport {
    /// Claim the writer for `state_key` on this Run's ComputeInstance.
    fn acquire_writer(&self, state_key: &str) -> Result<StateWriterGrant>;

    /// Download the artifact named by a content address into a temporary file
    /// created under `scratch`.
    ///
    /// The artifact is returned still encoded; verification (of the
    /// UNCOMPRESSED tar's digest) and expansion belong to the caller, which
    /// streams both. `scratch` is on the filesystem the tree will be expanded
    /// on, so the temporary file never doubles the disk cost elsewhere.
    fn download(&self, artifact_digest: &str, scratch: &Path) -> Result<DownloadedStateArtifact>;

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

    /// Keep the slot away from every future writer: this Run's workload could
    /// not be confirmed stopped, so it may still be writing. Unlike release or
    /// abort, nothing about this gives the slot back; only a later confirmed
    /// stop (Runner recovery) or an operator does.
    ///
    /// No default: a transport that silently released here would be exactly
    /// the bug this exists to prevent.
    fn quarantine_writer(&self, state_key: &str, writer_fence: u64, reason: &str) -> Result<()>;

    /// Report on a Runner-local volume this Run holds the writer for.
    /// Refused by default: a transport that cannot report cannot run a
    /// volume-backed Run, and saying so is the safe answer.
    fn report_volume(
        &self,
        _state_key: &str,
        _writer_fence: u64,
        _report: &VolumeReport,
    ) -> Result<()> {
        anyhow::bail!("this state transport cannot report on Runner-local volumes")
    }

    /// Report the outcome of the volume operation this lease carries
    /// (checkpoint, restore, delete). Refused by default, like
    /// `report_volume`.
    fn complete_volume_operation(
        &self,
        _operation_id: &str,
        _outcome: VolumeOperationOutcome,
        _error: Option<&str>,
    ) -> Result<()> {
        anyhow::bail!("this state transport cannot report volume operations")
    }
}

/// What a Runner reports about a volume operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeOperationOutcome {
    Succeeded,
    Failed,
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

#[cfg(test)]
mod execution_deadline_tests {
    use super::*;
    use crate::control::{ExecutionControl, now_ms};
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        time::Duration,
    };

    #[test]
    fn expired_restore_sends_no_request_but_confirmed_cleanup_can_continue() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let transport = LeaseStateArtifactTransport::new(
            reqwest::blocking::Client::new(),
            format!("http://{}", listener.local_addr().unwrap()),
            "lease_owned_test",
            "owned-test-token",
        );
        let control = ExecutionControl::new(now_ms().saturating_sub(1));
        let bounded = transport.with_execution_control(&control, Duration::from_secs(2));
        assert!(bounded.acquire_writer("app_data").is_err());
        assert!(
            bounded
                .download("sha256:owned-test-artifact", Path::new("unused"))
                .is_err()
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
            first
        });
        bounded.release_writer("app_data", 1).unwrap();
        assert!(
            server
                .join()
                .unwrap()
                .starts_with("POST /v1/runner-leases/lease_owned_test/state/writers/release ")
        );
    }
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

impl LeaseStateArtifactTransport {
    /// Bound preparation requests to the same execution clock. The explicit
    /// request ceiling is also narrowed; cleanup and commit remain available.
    pub fn with_execution_control<'a>(
        &'a self,
        control: &'a crate::control::ExecutionControl,
        request_ceiling: std::time::Duration,
    ) -> ExecutionStateArtifactTransport<'a> {
        ExecutionStateArtifactTransport {
            transport: self,
            control,
            request_ceiling,
        }
    }
    fn acquire_writer_request(
        &self,
        state_key: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<StateWriterGrant> {
        let request = self
            .client
            .post(self.url("writers"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "state_key": state_key }));
        let request = match timeout {
            Some(t) => request.timeout(t),
            None => request,
        };
        let response = request
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
        let volume = match body.get("volume") {
            None | Some(serde_json::Value::Null) => None,
            Some(volume) => Some(GrantedVolume {
                volume_ref: volume
                    .get("volume_ref")
                    .and_then(serde_json::Value::as_str)
                    .context("writer grant names a volume without its reference")?
                    .to_owned(),
                status: volume
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .and_then(super::volume::VolumeStatus::parse)
                    .context("writer grant names a volume with an unknown status")?,
            }),
        };
        Ok(StateWriterGrant {
            revision_ref,
            artifact_digest,
            writer_fence,
            volume,
        })
    }

    fn download_request(
        &self,
        artifact_digest: &str,
        timeout: Option<std::time::Duration>,
        scratch: &Path,
    ) -> Result<DownloadedStateArtifact> {
        let request = self
            .client
            .get(self.url(&format!("artifacts/{artifact_digest}")))
            .bearer_auth(&self.token)
            .header(
                "x-ato-accept-artifact-encoding",
                ArtifactEncoding::Gzip.header_value(),
            );
        let request = match timeout {
            Some(t) => request.timeout(t),
            None => request,
        };
        let response = request
            .send()?
            .error_for_status()
            .context("failed to download the state artifact")?;
        // A control plane that stored the revision before gzip existed answers
        // with the plain tar (or no encoding header at all); the digest is over
        // the decoded bytes in either case.
        let encoding = match response.headers().get("x-ato-artifact-encoding") {
            None => ArtifactEncoding::Identity,
            Some(value) => ArtifactEncoding::parse(
                value
                    .to_str()
                    .context("state artifact encoding header is not text")?,
            )?,
        };

        let limits = StateArtifactLimits::from_env();
        if let Some(length) = response.content_length() {
            ensure_encoded_within(length, &limits)?;
        }
        let mut file = tempfile::Builder::new()
            .prefix(".ato-state-download-")
            .tempfile_in(scratch)
            .context("failed to create the downloaded state artifact file")?;
        let mut body = response.take(limits.encoded_bytes.saturating_add(1));
        let written = std::io::copy(&mut body, file.as_file_mut())
            .context("failed to read the state artifact body")?;
        ensure_encoded_within(written, &limits)?;
        file.as_file_mut()
            .flush()
            .context("failed to flush the downloaded state artifact")?;
        Ok(DownloadedStateArtifact { file, encoding })
    }
}

/// A borrowed execution view; it never constructs or renews a deadline.
/// Writer release, quarantine and state commit delegate to reporting transport.
pub struct ExecutionStateArtifactTransport<'a> {
    transport: &'a LeaseStateArtifactTransport,
    control: &'a crate::control::ExecutionControl,
    request_ceiling: std::time::Duration,
}
impl StateArtifactTransport for ExecutionStateArtifactTransport<'_> {
    fn acquire_writer(&self, state_key: &str) -> Result<StateWriterGrant> {
        use crate::control::AttemptPhase;
        let _phase = self.control.phase(AttemptPhase::Source)?;
        self.transport.acquire_writer_request(
            state_key,
            Some(
                self.control
                    .cap(AttemptPhase::Source, self.request_ceiling)?,
            ),
        )
    }
    fn download(&self, artifact_digest: &str, scratch: &Path) -> Result<DownloadedStateArtifact> {
        use crate::control::AttemptPhase;
        let _phase = self.control.phase(AttemptPhase::Source)?;
        self.transport.download_request(
            artifact_digest,
            Some(
                self.control
                    .cap(AttemptPhase::Source, self.request_ceiling)?,
            ),
            scratch,
        )
    }
    fn commit(
        &self,
        key: &str,
        fence: u64,
        parent: Option<&str>,
        request: &str,
        artifact: &StateArtifact,
    ) -> Result<String> {
        self.transport.commit(key, fence, parent, request, artifact)
    }
    fn release_writer(&self, key: &str, fence: u64) -> Result<()> {
        self.transport.release_writer(key, fence)
    }
    fn abort_writer(&self, key: &str, fence: u64) -> Result<()> {
        self.transport.abort_writer(key, fence)
    }
    fn quarantine_writer(&self, key: &str, fence: u64, reason: &str) -> Result<()> {
        self.transport.quarantine_writer(key, fence, reason)
    }
    fn report_volume(&self, key: &str, fence: u64, report: &VolumeReport) -> Result<()> {
        self.transport.report_volume(key, fence, report)
    }
    fn complete_volume_operation(
        &self,
        id: &str,
        outcome: VolumeOperationOutcome,
        error: Option<&str>,
    ) -> Result<()> {
        self.transport.complete_volume_operation(id, outcome, error)
    }
}

impl StateArtifactTransport for LeaseStateArtifactTransport {
    fn acquire_writer(&self, state_key: &str) -> Result<StateWriterGrant> {
        self.acquire_writer_request(state_key, None)
    }
    fn download(&self, artifact_digest: &str, scratch: &Path) -> Result<DownloadedStateArtifact> {
        self.download_request(artifact_digest, None, scratch)
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

    fn quarantine_writer(&self, state_key: &str, writer_fence: u64, reason: &str) -> Result<()> {
        self.client
            .post(self.url("writers/quarantine"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "state_key": state_key,
                "writer_fence": writer_fence,
                "reason": reason.chars().take(500).collect::<String>(),
            }))
            .send()?
            .error_for_status()
            .context("failed to quarantine the state writer")?;
        Ok(())
    }

    fn report_volume(
        &self,
        state_key: &str,
        writer_fence: u64,
        report: &VolumeReport,
    ) -> Result<()> {
        let (suffix, body) = match report {
            VolumeReport::Ready { volume_ref } => (
                "volumes/ready",
                serde_json::json!({
                    "state_key": state_key,
                    "writer_fence": writer_fence,
                    "volume_ref": volume_ref,
                }),
            ),
            VolumeReport::Missing { volume_ref, reason } => (
                "volumes/missing",
                serde_json::json!({
                    "state_key": state_key,
                    "writer_fence": writer_fence,
                    "volume_ref": volume_ref,
                    "reason": reason.chars().take(500).collect::<String>(),
                }),
            ),
            VolumeReport::Usage {
                volume_ref,
                usage_bytes,
            } => (
                "volumes/usage",
                serde_json::json!({
                    "state_key": state_key,
                    "writer_fence": writer_fence,
                    "volume_ref": volume_ref,
                    "usage_bytes": usage_bytes,
                }),
            ),
        };
        self.client
            .post(self.url(suffix))
            .bearer_auth(&self.token)
            .json(&body)
            .send()?
            .error_for_status()
            .with_context(|| format!("failed to report the state volume ({suffix})"))?;
        Ok(())
    }

    fn complete_volume_operation(
        &self,
        operation_id: &str,
        outcome: VolumeOperationOutcome,
        error: Option<&str>,
    ) -> Result<()> {
        self.client
            .post(self.url("volume-operations/complete"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "outcome": match outcome {
                    VolumeOperationOutcome::Succeeded => "succeeded",
                    VolumeOperationOutcome::Failed => "failed",
                },
                "error": error.map(|error| error.chars().take(500).collect::<String>()),
            }))
            .send()?
            .error_for_status()
            .context("failed to report the volume operation")?;
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
                "x-ato-artifact-encoding",
                artifact.encoding().header_value(),
            )
            .header(
                "x-ato-parent-revision-ref",
                parent_revision_ref.unwrap_or(""),
            )
            .body(reqwest::blocking::Body::from(artifact.reader()?))
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
    // The artifact streams into a temporary file ON THE DESTINATION'S
    // FILESYSTEM, beside the directory it will be expanded into; neither the
    // encoded body nor the decoded tar is held in memory.
    let scratch = destination
        .parent()
        .context("state destination has no parent directory")?;
    std::fs::create_dir_all(scratch)
        .with_context(|| format!("failed to create {}", scratch.display()))?;
    let downloaded = transport.download(digest, scratch)?;
    downloaded.unpack(digest, destination)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;
    use std::process::Command;

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

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("gzip");
        encoder.finish().expect("gzip")
    }

    fn decoded(artifact: &StateArtifact) -> Vec<u8> {
        let mut decoder = GzDecoder::new(std::fs::File::open(artifact.file.path()).expect("open"));
        let mut bytes = Vec::new();
        decoder.read_to_end(&mut bytes).expect("decode");
        bytes
    }

    #[test]
    fn a_state_tree_round_trips_through_gzip_with_the_same_digest() {
        let source = fixture();
        let canonical = pack_canonical_state_tree(source.path()).expect("packs");
        let artifact = pack_state_tree(source.path()).expect("packs");

        // The identity did not move: gzip is transport, and the digest is
        // still the sha256 of the UNCOMPRESSED canonical tar. A tree packed by
        // this Runner is the same revision it always was.
        assert_eq!(artifact.encoding(), ArtifactEncoding::Gzip);
        assert_eq!(artifact.digest(), state_artifact_digest(&canonical));
        assert_eq!(decoded(&artifact), canonical);
        let encoded = artifact.encoded_bytes().expect("bytes");
        assert!(
            encoded.len() < canonical.len(),
            "the compressed artifact is not smaller"
        );

        let restored = tempfile::tempdir().expect("tempdir");
        let target = restored.path().join("state");
        DownloadedStateArtifact::from_encoded_bytes(ArtifactEncoding::Gzip, encoded)
            .expect("download")
            .unpack(artifact.digest(), &target)
            .expect("unpacks");
        assert_eq!(
            std::fs::read_to_string(target.join("app.db")).expect("read"),
            "sqlite-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("nested/notes.txt")).expect("read"),
            "hello"
        );
        // Round-tripping is stable: repacking the restored tree lands on the
        // same address, which is what makes the digest a revision identity.
        assert_eq!(
            pack_state_tree(&target).expect("repacks").digest(),
            artifact.digest()
        );
    }

    #[test]
    fn an_identity_artifact_from_an_older_runner_still_unpacks() {
        // Revisions stored before gzip existed are the plain tar, with no
        // encoding header. The digest is over those bytes, so they restore
        // unchanged.
        let source = fixture();
        let canonical = pack_canonical_state_tree(source.path()).expect("packs");
        let digest = state_artifact_digest(&canonical);
        let restored = tempfile::tempdir().expect("tempdir");
        let target = restored.path().join("state");
        let downloaded =
            DownloadedStateArtifact::from_encoded_bytes(ArtifactEncoding::Identity, canonical)
                .expect("download");
        assert_eq!(downloaded.encoding(), ArtifactEncoding::Identity);
        downloaded.unpack(&digest, &target).expect("unpacks");
        assert_eq!(
            std::fs::read_to_string(target.join("app.db")).expect("read"),
            "sqlite-bytes"
        );
    }

    #[test]
    fn a_gzip_body_whose_uncompressed_digest_differs_is_refused() {
        let source = fixture();
        let artifact = pack_state_tree(source.path()).expect("packs");
        let other = tempfile::tempdir().expect("tempdir");
        write(other.path(), "different.txt", "different");
        let other_digest =
            state_artifact_digest(&pack_canonical_state_tree(other.path()).expect("packs"));
        assert_ne!(artifact.digest(), other_digest);

        let destination = tempfile::tempdir().expect("tempdir");
        let target = destination.path().join("state");
        let error = DownloadedStateArtifact::from_encoded_bytes(
            ArtifactEncoding::Gzip,
            artifact.encoded_bytes().expect("bytes"),
        )
        .expect("download")
        .unpack(&other_digest, &target)
        .unwrap_err();
        assert!(error.to_string().contains("digest mismatch"), "{error}");
        // Fail closed: the check runs before any byte is written.
        assert!(!target.exists());
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

        let error = DownloadedStateArtifact::from_encoded_bytes(
            ArtifactEncoding::Gzip,
            artifact.encoded_bytes().expect("bytes"),
        )
        .expect("download")
        .unpack(&wrong, destination.path())
        .unwrap_err();
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

    #[cfg(unix)]
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
            fn download(&self, _digest: &str, _scratch: &Path) -> Result<DownloadedStateArtifact> {
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
            fn quarantine_writer(&self, _key: &str, _fence: u64, _reason: &str) -> Result<()> {
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
                volume: None,
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
        DownloadedStateArtifact::from_encoded_bytes(
            ArtifactEncoding::Gzip,
            artifact.encoded_bytes().expect("bytes"),
        )
        .expect("download")
        .unpack(artifact.digest(), &target)
        .expect("unpacks");
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
        let oversized = vec![0u8; DEFAULT_STATE_ARTIFACT_MAX_BYTES as usize + 1];
        let error =
            unpack_state_tree(&oversized, &state_artifact_digest(&oversized), &target).unwrap_err();
        assert!(error.to_string().contains("over the"), "{error}");
        assert!(
            error.to_string().contains(STATE_ARTIFACT_MAX_BYTES_ENV),
            "{error}"
        );
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn readonly_workspace_modes_stop_at_the_mount_root() {
        use std::os::unix::fs::PermissionsExt as _;
        // A umask applies to the whole process. Run this case alone so other
        // parallel libtest cases keep their own process's umask.
        const CHILD: &str = "ATO_TEST_READONLY_WORKSPACE_UMASK";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new("sh")
                .args(["-c", "umask 077; exec \"$@\"", "sh"])
                .arg(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg(std::thread::current().name().unwrap())
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        let private = tempfile::tempdir().unwrap();
        std::fs::set_permissions(private.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = private.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = private.path().join("workspace");

        // Unlike an explicit directory entry, this tar implicitly creates
        // both ancestors of the CA file using the Runner's process umask.
        let bytes = archive_with_entry("ca/nested/root.pem");
        unpack_state_tree(&bytes, &state_artifact_digest(&bytes), &state).unwrap();
        unpack_workspace_tree(&bytes, &state_artifact_digest(&bytes), &target).unwrap();
        for directory in [target.clone(), target.join("ca"), target.join("ca/nested")] {
            assert_eq!(mode(&directory), 0o755);
        }
        assert_eq!(mode(&target.join("ca/nested/root.pem")), 0o644);
        assert_eq!(mode(private.path()), 0o700);
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join("ca/nested")), 0o700);

        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("empty")).unwrap();
        let program = source.path().join("program");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let canonical = pack_canonical_state_tree(source.path()).unwrap();
        unpack_workspace_tree(&canonical, &state_artifact_digest(&canonical), &target).unwrap();
        assert_eq!(mode(&target.join("empty")), 0o755);
        assert_eq!(mode(&target.join("program")), 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_survives_the_round_trip() {
        // The packer records the owner-execute bit and the unpacker used to
        // drop it, so a materialized workspace arrived with a non-executable
        // interpreter. The Run then failed with a bare exit code and nothing
        // to read — which is how this was actually found.
        use std::os::unix::fs::PermissionsExt as _;

        let source = tempfile::tempdir().expect("tempdir");
        let program = source.path().join("bin/python");
        std::fs::create_dir_all(program.parent().expect("parent")).expect("mkdir");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(source.path().join("data.txt"), "plain").expect("write");

        let artifact = pack_state_tree(source.path()).expect("packs");
        let restored = tempfile::tempdir().expect("tempdir");
        let target = restored.path().join("tree");
        DownloadedStateArtifact::from_encoded_bytes(
            ArtifactEncoding::Gzip,
            artifact.encoded_bytes().expect("bytes"),
        )
        .expect("download")
        .unpack(artifact.digest(), &target)
        .expect("unpacks");

        let mode = std::fs::metadata(target.join("bin/python"))
            .expect("stat")
            .permissions()
            .mode();
        assert!(mode & 0o100 != 0, "the interpreter lost its execute bit");
        // And a plain file does not gain one.
        let plain = std::fs::metadata(target.join("data.txt"))
            .expect("stat")
            .permissions()
            .mode();
        assert!(plain & 0o111 == 0, "a data file must not become executable");
    }

    #[test]
    fn a_compressible_archive_over_the_tree_limit_is_refused_while_expanding() {
        // 8 MiB of zeros is a handful of KiB compressed: the ENCODED limit is
        // not what refuses this. The limit that protects the Runner's disk is
        // the uncompressed one, counted while the artifact streams.
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("zeros"), vec![0u8; 8 * 1024 * 1024]).expect("write");
        let canonical = pack_canonical_state_tree(source.path()).expect("packs");
        let encoded = gzip(&canonical);
        assert!(
            encoded.len() < canonical.len() / 100,
            "fixture is not compressible enough to prove the decoded limit"
        );

        let limits = StateArtifactLimits::from_raw(Some("67108864"), Some("1048576"));
        let artifact = EncodedArtifact {
            source: ArtifactSource::Bytes(&encoded),
            encoding: ArtifactEncoding::Gzip,
            encoded_len: encoded.len() as u64,
        };
        let destination = tempfile::tempdir().expect("tempdir");
        let target = destination.path().join("state");
        let error = unpack_state_artifact(
            &artifact,
            &state_artifact_digest(&canonical),
            &target,
            &limits,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(STATE_TREE_MAX_BYTES_ENV),
            "{error}"
        );
        assert!(!target.exists());
    }

    #[test]
    fn an_artifact_over_the_encoded_limit_is_refused() {
        let source = fixture();
        let artifact = pack_state_tree(source.path()).expect("packs");
        let limits = StateArtifactLimits::from_raw(Some("8"), Some("268435456"));

        // Producing one this Runner may not send is refused...
        let error = pack_state_tree_with_limits(source.path(), &limits).unwrap_err();
        assert!(
            error.to_string().contains(STATE_ARTIFACT_MAX_BYTES_ENV),
            "{error}"
        );

        // ...and so is receiving one.
        let encoded = artifact.encoded_bytes().expect("bytes");
        let received = EncodedArtifact {
            source: ArtifactSource::Bytes(&encoded),
            encoding: ArtifactEncoding::Gzip,
            encoded_len: encoded.len() as u64,
        };
        let destination = tempfile::tempdir().expect("tempdir");
        let target = destination.path().join("state");
        let error =
            unpack_state_artifact(&received, artifact.digest(), &target, &limits).unwrap_err();
        assert!(
            error.to_string().contains(STATE_ARTIFACT_MAX_BYTES_ENV),
            "{error}"
        );
        assert!(!target.exists());
    }

    #[test]
    fn both_limits_are_read_from_the_environment() {
        // A child process, like the umask case above: the limits come from the
        // process environment, so overriding them in a parallel test would
        // change what every other test packs.
        const CHILD: &str = "ATO_TEST_STATE_ARTIFACT_LIMITS_CHILD";
        let Some(case) = std::env::var_os(CHILD).map(|value| value.to_string_lossy().into_owned())
        else {
            let executable = std::env::current_exe().expect("test executable");
            let name = std::thread::current().name().expect("test name").to_owned();
            let run = |case: &str, artifact: &str, tree: &str| {
                let status = Command::new(&executable)
                    .args(["--exact", &name])
                    .env(CHILD, case)
                    .env(STATE_ARTIFACT_MAX_BYTES_ENV, artifact)
                    .env(STATE_TREE_MAX_BYTES_ENV, tree)
                    .status()
                    .expect("child runs");
                assert!(status.success(), "{case} child failed");
            };
            run("encoded", "64", "1048576");
            run("tree", "67108864", "512");
            run("defaults", "not-a-number", "");
            return;
        };

        let limits = StateArtifactLimits::from_env();
        let source = tempfile::tempdir().expect("tempdir");
        write(source.path(), "state.txt", "state");
        match case.as_str() {
            "encoded" => {
                assert_eq!(limits.encoded_bytes(), 64);
                assert_eq!(limits.tree_bytes(), 1_048_576);
                let error = pack_state_tree(source.path()).unwrap_err();
                assert!(
                    error.to_string().contains(STATE_ARTIFACT_MAX_BYTES_ENV),
                    "{error}"
                );
            }
            "tree" => {
                assert_eq!(limits.encoded_bytes(), DEFAULT_STATE_ARTIFACT_MAX_BYTES);
                assert_eq!(limits.tree_bytes(), 512);
                let error = pack_state_tree(source.path()).unwrap_err();
                assert!(
                    error.to_string().contains(STATE_TREE_MAX_BYTES_ENV),
                    "{error}"
                );
            }
            "defaults" => {
                assert_eq!(limits.encoded_bytes(), DEFAULT_STATE_ARTIFACT_MAX_BYTES);
                assert_eq!(limits.tree_bytes(), DEFAULT_STATE_TREE_MAX_BYTES);
                // The fallback is usable, not a silent zero.
                pack_state_tree(source.path()).expect("packs under the defaults");
            }
            other => panic!("unknown child case {other}"),
        }
    }
}
