//! 6b-D2: bounded source Dockerfile -> OCI artifact materialization.
//!
//! Ato owns the inputs (digest-verified source closure, the docker-default
//! root `Dockerfile`, platform, frozen base-image archives, build policy) and
//! the verification of the output. Dockerfile semantics belong to an existing
//! builder (BuildKit through a *private* Docker daemon session, see
//! [`session`]); nothing here parses a Dockerfile or rewrites it. The produced
//! archive is checked with the same `verify_oci_archive` the portable OCI
//! route already uses, and the image's own config (Cmd, ExposedPorts,
//! WorkingDir, Volumes) is read from the verified artifact, never from
//! Dockerfile text.
//!
//! Identity: inputs are recorded before the build; manifest/config/layer and
//! archive digests are recorded only after it. Reproducibility is not claimed.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ato_formation::source::{DownloadedArchive, SourceLimits, TreeVerifiedArchive};
use ato_objects::PortableOciArchive;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::oci_archive::{
    BaseGraph, MAX_BASE_ARCHIVE_BYTES, ScanLimits, blob_path, member_json, scan_members,
    verify_base_archive, verify_oci_archive,
};

pub mod isolation;
#[cfg(target_os = "linux")]
pub mod session;

pub const SOURCE_OCI_REQUEST_SCHEMA: &str = "ato.source-oci-request/1";
pub const SOURCE_OCI_PROVENANCE_SCHEMA: &str = "ato.source-oci-materialization/1";
/// Same bound as portable OCI transport.
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_BUILD_TIMEOUT_SECONDS: u64 = 3600;
pub const MAX_BASE_IMAGES: usize = 8;
/// Build progress output kept by the client before the build is aborted.
pub const MAX_BUILD_LOG_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_BUILD_DISK_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_BUILD_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MIN_BUILD_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
pub const MIN_BUILD_PIDS: u64 = 64;
/// Only the docker-default selection is authorized in v0.
pub const SELECTED_DOCKERFILE: &str = "Dockerfile";

/// A typed refusal. When releasing the builder also failed, that second
/// failure is kept next to the original one instead of replacing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOciError {
    pub code: &'static str,
    pub detail: String,
    pub cleanup: Option<Box<SourceOciError>>,
}
impl fmt::Display for SourceOciError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)?;
        if let Some(cleanup) = &self.cleanup {
            write!(f, "; cleanup: {cleanup}")?;
        }
        Ok(())
    }
}
impl std::error::Error for SourceOciError {}

pub(crate) fn err(code: &'static str, detail: impl Into<String>) -> SourceOciError {
    SourceOciError {
        code,
        detail: detail.into(),
        cleanup: None,
    }
}
pub type Result<T> = std::result::Result<T, SourceOciError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOciRequest {
    pub schema: String,
    pub title: String,
    pub source_archive: PathBuf,
    pub source_archive_sha256: String,
    pub dockerfile: String,
    pub platform: String,
    /// Every external image the build may use, frozen before the build.
    pub base_images: Vec<BaseImageInput>,
    /// Explicit port binding; must equal the built image's single ExposedPort.
    pub declared_transport_port: u16,
    pub policy: SourceOciPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseImageInput {
    /// The reference exactly as the Dockerfile's FROM names it (a tag or a
    /// digest reference). It is only a lookup key for the build.
    pub reference: String,
    /// The frozen root digest Ato resolved that reference to, before the
    /// build. A digest written in `reference` must equal it.
    pub pinned_digest: String,
    pub archive: PathBuf,
    pub archive_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOciPolicy {
    /// v0 accepts only "none": build-time acquisition is a separate opt-in.
    pub network: String,
    pub build_timeout_seconds: u64,
    pub max_archive_bytes: u64,
    /// Limits of the whole private builder session (daemon, BuildKit and
    /// every RUN step), enforced while the build runs.
    pub build: BuildLimits,
    /// Limits the authored OCI route runs with.
    pub runtime: RuntimeLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLimits {
    pub memory_bytes: u64,
    pub cpu_limit_millis: u64,
    pub pids_limit: u64,
    pub disk_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLimits {
    pub memory_bytes: u64,
    pub cpu_limit_millis: u64,
    pub pids_limit: u64,
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A FROM-style image reference: no whitespace, no build-arg expansion, no
/// `=` (the named-context separator). A digest part, when present, is returned.
fn parse_reference(reference: &str) -> Option<Option<&str>> {
    if reference.is_empty()
        || reference.len() > 256
        || reference.contains(char::is_whitespace)
        || reference.contains(['$', '=', '\0'])
    {
        return None;
    }
    match reference.rsplit_once('@') {
        Some((name, digest)) => (!name.is_empty() && is_digest(digest)).then_some(Some(digest)),
        None => Some(None),
    }
}

impl SourceOciRequest {
    pub fn validate(&self) -> Result<()> {
        let bad = |detail: &str| Err(err("source_oci_request_invalid", detail));
        if self.schema != SOURCE_OCI_REQUEST_SCHEMA {
            return bad("unknown request schema");
        }
        if self.title.trim().is_empty() || self.title.len() > 120 {
            return bad("title must be 1..=120 bytes");
        }
        if !is_digest(&self.source_archive_sha256) {
            return bad("source archive digest must be sha256:<hex>");
        }
        if self.dockerfile != SELECTED_DOCKERFILE {
            return Err(err(
                "source_oci_dockerfile_unselected",
                "v0 builds only the docker-default root `Dockerfile`",
            ));
        }
        if !matches!(self.platform.as_str(), "linux/amd64" | "linux/arm64") {
            return bad("platform must be linux/amd64 or linux/arm64");
        }
        if self.declared_transport_port == 0 {
            return bad("declared transport port must be 1..=65535");
        }
        if self.base_images.len() > MAX_BASE_IMAGES {
            return bad("too many base images");
        }
        let mut seen = BTreeSet::new();
        for base in &self.base_images {
            let written = parse_reference(&base.reference);
            if written.is_none()
                || !is_digest(&base.pinned_digest)
                || written.flatten().is_some_and(|d| d != base.pinned_digest)
                || !seen.insert(&base.reference)
            {
                return Err(err(
                    "source_oci_base_reference_invalid",
                    "base references must be unique and resolved to a pinned root digest",
                ));
            }
            if !is_digest(&base.archive_sha256) {
                return bad("base archive digest must be sha256:<hex>");
            }
        }
        let p = &self.policy;
        if p.network != "none" {
            return Err(err(
                "source_oci_network_unauthorized",
                "build network must be \"none\"; online acquisition needs its own explicit policy",
            ));
        }
        let b = &p.build;
        let r = &p.runtime;
        if !(1..=MAX_BUILD_TIMEOUT_SECONDS).contains(&p.build_timeout_seconds)
            || !(1..=MAX_ARCHIVE_BYTES).contains(&p.max_archive_bytes)
            || b.memory_bytes < MIN_BUILD_MEMORY_BYTES
            || !(100..=64_000).contains(&b.cpu_limit_millis)
            || b.pids_limit < MIN_BUILD_PIDS
            || !(MIN_BUILD_DISK_BYTES..=MAX_BUILD_DISK_BYTES).contains(&b.disk_bytes)
            || r.memory_bytes == 0
            || r.cpu_limit_millis == 0
            || r.pids_limit == 0
        {
            return bad("policy bounds");
        }
        Ok(())
    }
}

/// Outcome of one builder invocation. The log tail is bounded evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildOutcome {
    Built,
    Failed {
        log_tail: String,
    },
    TimedOut,
    /// The build wrote more progress output than [`MAX_BUILD_LOG_BYTES`].
    LogExceeded,
}

/// The narrow surface Ato needs from an existing builder. Every method after
/// `scratch` may change builder state; [`OciBuilder::release`] must stop all
/// build work and confirm that no resource the builder owns remains.
pub trait OciBuilder {
    /// Bounded, builder-owned working directory for this one job.
    fn scratch(&self) -> &Path;
    fn identity(&self) -> Result<Value>;
    fn image_ids(&self) -> Result<Vec<String>>;
    fn load(&self, archive: &Path) -> Result<()>;
    fn tag(&self, image: &str, tag: &str) -> Result<()>;
    fn build(
        &self,
        context: &Path,
        platform: &str,
        named_contexts: &[(String, String)],
        tag: &str,
        timeout: Duration,
    ) -> Result<BuildOutcome>;
    fn image_id(&self, reference: &str) -> Result<String>;
    /// Uncompressed size the builder reports; checked before any save.
    fn image_size(&self, image_id: &str) -> Result<u64>;
    fn save(&self, image_id: &str, output: &Path) -> Result<()>;
    fn release(&self) -> Result<()>;
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn tail(text: &str, bytes: usize) -> String {
    let start = text.len().saturating_sub(bytes);
    let start = (start..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let file = File::open(path).map_err(|e| {
        err(
            "source_oci_input_unreadable",
            format!("{}: {e}", path.display()),
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buffer[..n]);
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), size))
}

/// Copy `from` to `to` while hashing, refusing more than `max` bytes.
fn copy_hashed(from: &Path, to: &Path, max: u64) -> Result<String> {
    let input = File::open(from).map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
    let mut reader = BufReader::new(input);
    let mut output =
        File::create(to).map_err(|e| err("source_oci_scratch_failed", e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > max {
            return Err(err(
                "source_oci_base_digest_mismatch",
                "base archive exceeds its bound",
            ));
        }
        hasher.update(&buffer[..n]);
        output
            .write_all(&buffer[..n])
            .map_err(|e| err("source_oci_scratch_failed", e.to_string()))?;
    }
    output
        .sync_all()
        .map_err(|e| err("source_oci_scratch_failed", e.to_string()))?;
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// The archive bytes must equal the frozen digest, and the pinned root must
/// reach (through the selected platform manifest) exactly the config and
/// layers the Docker load manifest names.
fn check_base_archive(base: &BaseImageInput, path: &Path, platform: &str) -> Result<BaseGraph> {
    let (digest, _) = sha256_file(path)?;
    if digest != base.archive_sha256 {
        return Err(err(
            "source_oci_base_digest_mismatch",
            format!(
                "{} archive bytes differ from the frozen digest",
                base.reference
            ),
        ));
    }
    let file = File::open(path).map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
    verify_base_archive(BufReader::new(file), &base.pinned_digest, platform).map_err(|e| {
        err(
            "source_oci_base_graph_invalid",
            format!("{}: {e:#}", base.reference),
        )
    })
}

/// Inputs verified before any builder exists: request bounds, the source
/// archive digest and tree, and every base archive's pinned graph.
pub struct Prepared {
    request: SourceOciRequest,
    source: TreeVerifiedArchive,
    closure: String,
    bases: Vec<BaseGraph>,
}

impl Prepared {
    pub fn request(&self) -> &SourceOciRequest {
        &self.request
    }
}

pub fn prepare(request: &SourceOciRequest) -> Result<Prepared> {
    request.validate()?;
    let bytes = std::fs::read(&request.source_archive)
        .map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
    let limits = SourceLimits::default();
    let source = DownloadedArchive::new(bytes)
        .verify_archive_digest(&request.source_archive_sha256)
        .and_then(|a| a.verify_tree_digest(None, limits))
        .map_err(|e| err("source_oci_source_invalid", e.to_string()))?;
    let closure = source
        .closure_ref("")
        .map_err(|e| err("source_oci_source_invalid", e.to_string()))?
        .as_str()
        .to_owned();
    let bases = request
        .base_images
        .iter()
        .map(|base| check_base_archive(base, &base.archive, &request.platform))
        .collect::<Result<Vec<_>>>()?;
    Ok(Prepared {
        request: request.clone(),
        source,
        closure,
        bases,
    })
}

/// What the verified artifact itself says; never read from Dockerfile text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageFacts {
    pub manifest_digest: String,
    pub config_digest: String,
    pub layer_digests: Vec<String>,
    pub os: String,
    pub architecture: String,
    pub cmd: Vec<String>,
    pub entrypoint: Vec<String>,
    pub exposed_ports: Vec<String>,
    pub working_dir: String,
    pub volumes: Vec<String>,
    pub user: String,
}

fn scan_saved(path: &Path, max_bytes: u64) -> Result<BTreeMap<String, crate::oci_archive::Member>> {
    let file = File::open(path).map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?;
    scan_members(
        BufReader::new(file),
        &ScanLimits {
            max_members: 1024,
            max_member_bytes: max_bytes,
            max_total_bytes: max_bytes,
            keep_bytes_up_to: 4 * 1024 * 1024,
        },
    )
    .map_err(|e| err("source_oci_artifact_invalid", format!("{e:#}")))
}

/// The one manifest the index names, and the config and layer digests it
/// references (descriptor digests only; bytes are checked by the validator).
fn saved_graph(
    members: &BTreeMap<String, crate::oci_archive::Member>,
) -> Result<(String, Value, String, Vec<String>)> {
    let invalid = |d: String| err("source_oci_artifact_invalid", d);
    let index =
        member_json(members, "index.json", "OCI index").map_err(|e| invalid(format!("{e:#}")))?;
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str()
        .filter(|d| is_digest(d) && index["manifests"].as_array().is_some_and(|m| m.len() == 1))
        .ok_or_else(|| invalid("index must name one manifest".into()))?
        .to_owned();
    let path = blob_path(&manifest_digest).map_err(|e| invalid(format!("{e:#}")))?;
    let manifest =
        member_json(members, &path, "OCI manifest").map_err(|e| invalid(format!("{e:#}")))?;
    let config = manifest["config"]["digest"]
        .as_str()
        .filter(|d| is_digest(d))
        .ok_or_else(|| invalid("config digest".into()))?
        .to_owned();
    let layers = manifest["layers"]
        .as_array()
        .ok_or_else(|| invalid("layers".into()))?
        .iter()
        .map(|l| {
            l["digest"]
                .as_str()
                .filter(|d| is_digest(d))
                .map(str::to_owned)
                .ok_or_else(|| invalid("layer digest".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((manifest_digest, manifest, config, layers))
}

pub fn image_facts(archive: &Path) -> Result<ImageFacts> {
    let members = scan_saved(archive, MAX_ARCHIVE_BYTES)?;
    let (manifest_digest, _, config_digest, layer_digests) = saved_graph(&members)?;
    let config = member_json(
        &members,
        &blob_path(&config_digest)
            .map_err(|e| err("source_oci_artifact_invalid", format!("{e:#}")))?,
        "OCI config",
    )
    .map_err(|e| err("source_oci_artifact_invalid", format!("{e:#}")))?;
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let keys = |v: &Value| -> Vec<String> {
        v.as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    };
    let c = &config["config"];
    Ok(ImageFacts {
        layer_digests,
        manifest_digest,
        config_digest,
        os: config["os"].as_str().unwrap_or_default().to_owned(),
        architecture: config["architecture"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        cmd: strings(&c["Cmd"]),
        entrypoint: strings(&c["Entrypoint"]),
        exposed_ports: keys(&c["ExposedPorts"]),
        working_dir: c["WorkingDir"].as_str().unwrap_or_default().to_owned(),
        volumes: keys(&c["Volumes"]),
        user: c["User"].as_str().unwrap_or_default().to_owned(),
    })
}

/// Writer that refuses to grow past a bound.
struct Bounded<W> {
    inner: W,
    written: u64,
    max: u64,
}
impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > self.max {
            return Err(std::io::Error::other("archive bound exceeded"));
        }
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reader that hashes what passes through it.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
}
impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// Repack a private-daemon `docker save` into the portable transport form:
/// only `oci-layout`, `index.json`, `manifest.json` and the blobs the single
/// OCI manifest references (manifest, config, layers). A classic image store
/// also writes legacy v1 per-layer JSON blobs that nothing references; the
/// portable validator refuses those. Blob bytes are streamed unchanged and
/// re-hashed while copied, so the manifest digest does not change. The output
/// never grows past `max_bytes`. Returns how many members were dropped.
pub fn repack_saved_archive(saved: &Path, output: &Path, max_bytes: u64) -> Result<usize> {
    let saved_len = std::fs::metadata(saved)
        .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?
        .len();
    let members = scan_saved(saved, saved_len.max(1))?;
    let (manifest_digest, _, config, layers) = saved_graph(&members)?;
    let invalid = |d: String| err("source_oci_artifact_invalid", d);
    let mut keep: BTreeSet<String> = ["oci-layout", "index.json", "manifest.json"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for digest in std::iter::once(&manifest_digest)
        .chain(std::iter::once(&config))
        .chain(layers.iter())
    {
        keep.insert(blob_path(digest).map_err(|e| invalid(format!("{e:#}")))?);
    }
    if let Some(missing) = keep.iter().find(|k| !members.contains_key(*k)) {
        return Err(invalid(format!("referenced member {missing} missing")));
    }
    let bounds = |e: std::io::Error| {
        if e.to_string().contains("archive bound exceeded") {
            err(
                "source_oci_artifact_bounds",
                format!("repacked archive exceeds {max_bytes} bytes"),
            )
        } else {
            invalid(e.to_string())
        }
    };
    let file = File::create(output).map_err(|e| invalid(e.to_string()))?;
    let mut tar = tar::Builder::new(Bounded {
        inner: std::io::BufWriter::new(file),
        written: 0,
        max: max_bytes,
    });
    for dir in ["blobs/", "blobs/sha256/"] {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_size(0);
        header.set_cksum();
        tar.append_data(&mut header, dir, std::io::empty())
            .map_err(bounds)?;
    }
    // Streamed in the saved archive's order; each kept member is re-hashed
    // while copied and must be the member the scan verified.
    let source = File::open(saved).map_err(|e| invalid(e.to_string()))?;
    let mut archive = tar::Archive::new(BufReader::new(source));
    let mut written = BTreeSet::new();
    for entry in archive.entries().map_err(|e| invalid(e.to_string()))? {
        let entry = entry.map_err(|e| invalid(e.to_string()))?;
        let name = entry
            .path()
            .map_err(|e| invalid(e.to_string()))?
            .to_string_lossy()
            .into_owned();
        if !entry.header().entry_type().is_file() || !keep.contains(&name) {
            continue;
        }
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_size(entry.size());
        header.set_cksum();
        let mut reader = Hashing {
            inner: entry,
            hasher: Sha256::new(),
        };
        tar.append_data(&mut header, &name, &mut reader)
            .map_err(bounds)?;
        let digest = format!("sha256:{:x}", reader.hasher.finalize());
        if members.get(&name).is_none_or(|m| m.digest != digest) || !written.insert(name.clone()) {
            return Err(invalid(format!("{name} changed between passes")));
        }
    }
    if written != keep {
        return Err(invalid("repacked members differ from the scan".into()));
    }
    let mut writer = tar.into_inner().map_err(bounds)?;
    writer.flush().map_err(bounds)?;
    Ok(members.len() - keep.len())
}

/// Guest path the OCI route mounts the (capsule-only) workspace at, chosen so
/// it cannot shadow the image's own filesystem.
pub const SOURCE_OCI_WORKSPACE_MOUNT: &str = "/.ato-workspace";
/// The current portable OCI profile runs every route with this workdir.
pub const PROFILE_WORKING_DIR: &str = "/app";
/// Written next to the authored capsule.toml; it is the route's (read-only)
/// workspace content, since the packer requires at least one workspace file.
pub const PROVENANCE_FILE: &str = "source-oci-provenance.json";
/// Published artifact name inside the output directory.
pub const ARCHIVE_FILE: &str = "image.tar";

#[derive(Debug, Clone, Serialize)]
pub struct Materialized {
    pub provenance: Value,
    pub capsule_toml: String,
    pub image_reference: String,
    pub archive: PathBuf,
}

/// Build one prepared source into one verified OCI archive plus an authored
/// `ato.capsule/2` OCI route. `out` must be a new directory. The builder is
/// released on every path; if that release cannot be confirmed, the result
/// is a failure that also carries the original outcome, and no artifact is
/// left in `out`.
pub fn materialize(
    prepared: &Prepared,
    builder: &dyn OciBuilder,
    out: &Path,
) -> Result<Materialized> {
    let created =
        std::fs::create_dir(out).map_err(|e| err("source_oci_output_invalid", e.to_string()));
    let result = created.and_then(|()| build_and_verify(prepared, builder, out));
    let released = builder.release();
    let outcome = match (result, released) {
        (Ok(m), Ok(())) => Ok(m),
        (Err(e), Ok(())) => Err(e),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(mut e), Err(cleanup)) => {
            e.cleanup = Some(Box::new(cleanup));
            Err(e)
        }
    };
    if outcome.is_err() && out.exists() {
        // Nothing is published from a failed or unconfirmed job.
        let _ = std::fs::remove_dir_all(out);
    }
    outcome
}

fn build_and_verify(
    prepared: &Prepared,
    builder: &dyn OciBuilder,
    out: &Path,
) -> Result<Materialized> {
    let request = &prepared.request;
    let scratch = builder.scratch().to_path_buf();
    // 1. Frozen source closure, materialized inside the bounded scratch.
    let limits = SourceLimits::default();
    let context = prepared
        .source
        .materialize(&scratch.join("context"), "", limits)
        .map_err(|e| err("source_oci_source_invalid", e.to_string()))?;
    let dockerfile = context.join(SELECTED_DOCKERFILE);
    let meta = std::fs::symlink_metadata(&dockerfile).map_err(|_| {
        err(
            "source_oci_dockerfile_absent",
            "no root Dockerfile in the source",
        )
    })?;
    if !meta.file_type().is_file() {
        return Err(err(
            "source_oci_dockerfile_absent",
            "root Dockerfile is not a regular file",
        ));
    }
    let dockerignore = context.join(".dockerignore");
    let dockerignore_sha256 = std::fs::symlink_metadata(&dockerignore)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|_| sha256_file(&dockerignore).map(|(d, _)| d))
        .transpose()?;
    // 2. Private, empty builder only.
    if !builder.image_ids()?.is_empty() {
        return Err(err(
            "source_oci_builder_store_not_empty",
            "the private builder store must be empty; no other job's images or cache",
        ));
    }
    let identity = builder.identity()?;
    // 3. Frozen external images: the exact bytes that were verified are the
    // bytes loaded (a private copy, re-verified), and the store must then
    // hold exactly the verified configs.
    let mut named = Vec::new();
    let mut bases = Vec::new();
    for (n, (base, graph)) in request.base_images.iter().zip(&prepared.bases).enumerate() {
        let copy = scratch.join(format!("base-{n}.tar"));
        let copied = copy_hashed(&base.archive, &copy, MAX_BASE_ARCHIVE_BYTES)?;
        if copied != base.archive_sha256 {
            return Err(err(
                "source_oci_base_digest_mismatch",
                format!("{} changed after it was verified", base.reference),
            ));
        }
        if check_base_archive(base, &copy, &request.platform)? != *graph {
            return Err(err(
                "source_oci_base_graph_invalid",
                format!("{} graph changed after it was verified", base.reference),
            ));
        }
        builder.load(&copy)?;
        let tag = format!("ato-base/b{n}:frozen");
        builder.tag(&graph.config, &tag)?;
        named.push((base.reference.clone(), format!("docker-image://{tag}")));
        bases.push(json!({
            "reference": base.reference,
            "archive_sha256": base.archive_sha256,
            "pinned_digest": base.pinned_digest,
            "platform_manifest_digest": graph.platform_manifest,
            "config_digest": graph.config,
            "layer_digests": graph.layers,
        }));
    }
    let loaded: BTreeSet<String> = builder.image_ids()?.into_iter().collect();
    let expected: BTreeSet<String> = prepared.bases.iter().map(|g| g.config.clone()).collect();
    if loaded != expected {
        return Err(err(
            "source_oci_base_graph_invalid",
            format!("store holds {loaded:?} after loading; verified configs are {expected:?}"),
        ));
    }
    // 4. Build with the existing builder; semantics are BuildKit's.
    let tag = "ato-source/build:materialize";
    let started = Instant::now();
    let outcome = builder.build(
        &context,
        &request.platform,
        &named,
        tag,
        Duration::from_secs(request.policy.build_timeout_seconds),
    )?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match outcome {
        BuildOutcome::Built => {}
        BuildOutcome::TimedOut => {
            return Err(err(
                "source_oci_build_timeout",
                format!("build exceeded {} s", request.policy.build_timeout_seconds),
            ));
        }
        BuildOutcome::LogExceeded => {
            return Err(err(
                "source_oci_log_bound",
                format!("build output exceeded {MAX_BUILD_LOG_BYTES} bytes"),
            ));
        }
        BuildOutcome::Failed { log_tail } => {
            return Err(err("source_oci_build_failed", log_tail));
        }
    }
    // 5. Export within bounds and verify with the existing portable validator.
    let max = request.policy.max_archive_bytes;
    let image_id = builder.image_id(tag)?;
    let reported = builder.image_size(&image_id)?;
    if reported > max {
        return Err(err(
            "source_oci_artifact_bounds",
            format!("image is {reported} bytes; the archive bound is {max}; not saved"),
        ));
    }
    let saved = scratch.join("image.saved.tar");
    builder.save(&image_id, &saved)?;
    let partial = out.join("image.tar.partial");
    let dropped = repack_saved_archive(&saved, &partial, max)?;
    std::fs::remove_file(&saved).map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?;
    let (archive_sha256, size) = sha256_file(&partial)?;
    let facts = image_facts(&partial)?;
    if facts.config_digest != image_id {
        return Err(err(
            "source_oci_artifact_digest_mismatch",
            "saved config differs from the built image",
        ));
    }
    let short = &prepared.closure[7..19];
    let image_reference = format!("ato-source/{short}@{}", facts.manifest_digest);
    let (os, arch) = request.platform.split_once('/').expect("validated");
    if facts.os != os || facts.architecture != arch {
        return Err(err(
            "source_oci_platform_mismatch",
            format!("{}/{}", facts.os, facts.architecture),
        ));
    }
    let portable = PortableOciArchive {
        image: image_reference.clone(),
        platform: request.platform.clone(),
        bytes: base64::engine::general_purpose::STANDARD.encode(
            std::fs::read(&partial)
                .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?,
        ),
    };
    let validated = verify_oci_archive(&portable)
        .map_err(|e| err("source_oci_artifact_digest_mismatch", format!("{e:#}")))?;
    if validated.config_reference != facts.config_digest {
        return Err(err(
            "source_oci_artifact_digest_mismatch",
            "validator config reference differs",
        ));
    }
    // 6. Explicit port binding against the artifact's own declaration.
    let expected = format!("{}/tcp", request.declared_transport_port);
    if facts.exposed_ports != [expected.clone()] {
        return Err(err(
            "source_oci_port_unmatched",
            format!(
                "image declares {:?}; the explicit binding is {expected}",
                facts.exposed_ports
            ),
        ));
    }
    // The portable OCI profile requires argv; copy the artifact's own Cmd
    // verbatim (OCI semantics; its ENTRYPOINT still applies). Never parse
    // Dockerfile text or translate it into a native process.
    if facts.cmd.is_empty() {
        return Err(err(
            "source_oci_cmd_absent",
            "the image has no Cmd; the current OCI profile needs argv (profile design gap)",
        ));
    }
    let mut profile_divergences = Vec::new();
    if !facts.working_dir.is_empty() && facts.working_dir != PROFILE_WORKING_DIR {
        profile_divergences.push(json!({"kind":"working_dir","image":facts.working_dir,
            "profile":PROFILE_WORKING_DIR,"effect":"the current OCI route runs with --workdir /app; relative Cmd paths resolve there"}));
    }
    if !facts.volumes.is_empty() {
        profile_divergences.push(json!({"kind":"volume","image":facts.volumes,
            "effect":"root filesystem is read-only and no state slot is bound; writes to these paths fail"}));
    }
    let capsule_toml = authored_route(request, &image_reference, &facts);
    let archive = out.join(ARCHIVE_FILE);
    std::fs::rename(&partial, &archive)
        .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?;
    let provenance = json!({
        "schema": SOURCE_OCI_PROVENANCE_SCHEMA,
        "inputs": {
            "source_archive_sha256": request.source_archive_sha256,
            "source_closure_ref": prepared.closure,
            "dockerfile": SELECTED_DOCKERFILE,
            "dockerfile_sha256": sha256_file(&dockerfile)?.0,
            "dockerignore_sha256": dockerignore_sha256,
            "build_context": ".",
            "target": "default (last stage)",
            "build_args": {},
            "platform": request.platform,
            "base_images": bases,
            "builder": identity,
            "policy": request.policy,
        },
        "outputs": {
            "image_reference": image_reference,
            "manifest_digest": facts.manifest_digest,
            "config_digest": facts.config_digest,
            "layer_digests": facts.layer_digests,
            "archive_sha256": archive_sha256,
            "archive_bytes": size,
            "repack": {"dropped_unreferenced_members": dropped,
                "rule": "keep oci-layout, index.json, manifest.json and blobs referenced by the one OCI manifest; blob bytes unchanged"},
            "image_config": {"cmd":facts.cmd,"entrypoint":facts.entrypoint,
                "exposed_ports":facts.exposed_ports,"working_dir":facts.working_dir,
                "volumes":facts.volumes,"user":facts.user},
        },
        "declared_transport_port": request.declared_transport_port,
        "http_suitability": "unverified until the Runtime and Verifier observe it",
        "profile_divergences": profile_divergences,
        "build_elapsed_ms": elapsed_ms,
        "reproducibility": "not claimed; outputs are observed digests of this build",
    });
    Ok(Materialized {
        provenance,
        capsule_toml,
        image_reference,
        archive,
    })
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string")
}

/// The Ato-owned authored route: one OCI serving step over the verified image.
/// K observes only GET / = 200 on the Surface; it is not a functional claim.
fn authored_route(request: &SourceOciRequest, image: &str, facts: &ImageFacts) -> String {
    let p = &request.policy.runtime;
    let argv = facts
        .cmd
        .iter()
        .map(|a| toml_string(a))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"schema = "ato.capsule/2"

[application]
title = {title}
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
path = "/"
status = 200

[[derivation]]
id = "source-oci"
use = "ato.oci@1"
argv = [{argv}]
cwd = "."
guest_port = {port}
runtimes = {{ "oci.image" = {image}, "oci.platform" = {platform}, "oci.memory_bytes" = "{memory}", "oci.cpu_limit_millis" = "{cpu}", "oci.pids_limit" = "{pids}", "oci.workspace_mount" = "{mount}" }}

[effects]
default = "pure"
"#,
        title = toml_string(&request.title),
        port = request.declared_transport_port,
        image = toml_string(image),
        platform = toml_string(&request.platform),
        memory = p.memory_bytes,
        cpu = p.cpu_limit_millis,
        pids = p.pids_limit,
        mount = SOURCE_OCI_WORKSPACE_MOUNT,
    )
}

#[cfg(test)]
mod tests;
