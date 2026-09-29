//! 6b-D2: bounded source Dockerfile -> OCI artifact materialization.
//!
//! Ato owns the inputs (digest-verified source closure, the docker-default
//! root `Dockerfile`, platform, frozen base-image archives, build policy) and
//! the verification of the output. Dockerfile semantics belong to an existing
//! builder (BuildKit through a *private* Docker daemon); nothing here parses a
//! Dockerfile or rewrites it. The produced archive is checked with the same
//! `verify_oci_archive` the portable OCI route already uses, and the image's
//! own config (Cmd, ExposedPorts, WorkingDir, Volumes) is read from the
//! verified artifact, never from Dockerfile text.
//!
//! Identity: inputs are recorded before the build; manifest/config/layer and
//! archive digests are recorded only after it. Reproducibility is not claimed.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ato_formation::source::{DownloadedArchive, SourceLimits};
use ato_objects::PortableOciArchive;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::oci_archive::verify_oci_archive;

pub const SOURCE_OCI_REQUEST_SCHEMA: &str = "ato.source-oci-request/1";
pub const SOURCE_OCI_PROVENANCE_SCHEMA: &str = "ato.source-oci-materialization/1";
/// Same bound as portable OCI transport.
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_BUILD_TIMEOUT_SECONDS: u64 = 3600;
pub const MAX_BASE_IMAGES: usize = 8;
/// Only the docker-default selection is authorized in v0.
pub const SELECTED_DOCKERFILE: &str = "Dockerfile";
const PRODUCTION_SOCKETS: &[&str] = &["/var/run/docker.sock", "/run/docker.sock"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct SourceOciError {
    pub code: &'static str,
    pub detail: String,
}
fn err(code: &'static str, detail: impl Into<String>) -> SourceOciError {
    SourceOciError {
        code,
        detail: detail.into(),
    }
}
type Result<T> = std::result::Result<T, SourceOciError>;

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
        if !(1..=MAX_BUILD_TIMEOUT_SECONDS).contains(&p.build_timeout_seconds)
            || !(1..=MAX_ARCHIVE_BYTES).contains(&p.max_archive_bytes)
            || p.memory_bytes == 0
            || p.cpu_limit_millis == 0
            || p.pids_limit == 0
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
    Failed { log_tail: String },
    TimedOut,
}

/// The narrow surface Ato needs from an existing builder.
pub trait OciBuilder {
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
    fn save(&self, image_id: &str, output: &Path) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// BuildKit through a caller-provided *private* Docker daemon. The production
/// daemon sockets are refused; the store must be empty before a build.
pub struct DockerCliBuilder {
    docker: PathBuf,
    host: String,
}

impl DockerCliBuilder {
    pub fn new(docker: PathBuf, host: &str) -> Result<Self> {
        let path = host.strip_prefix("unix://").ok_or_else(|| {
            err(
                "source_oci_builder_socket_refused",
                "the builder needs an explicit private unix:// Docker socket",
            )
        })?;
        if !path.starts_with('/') || PRODUCTION_SOCKETS.contains(&path) {
            return Err(err(
                "source_oci_builder_socket_refused",
                "the host's production Docker socket is never given to a source build",
            ));
        }
        Ok(Self {
            docker,
            host: host.to_owned(),
        })
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.docker);
        // Only the private daemon; never inherit proxies or another context.
        command
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("DOCKER_HOST", &self.host)
            .env("HOME", std::env::temp_dir())
            .env("DOCKER_BUILDKIT", "1");
        command
    }
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        if !output.status.success() {
            return Err(err(
                "source_oci_builder_failed",
                tail(&String::from_utf8_lossy(&output.stderr), 2000),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn tail(text: &str, bytes: usize) -> String {
    let start = text.len().saturating_sub(bytes);
    let start = (start..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    text[start..].to_owned()
}

impl OciBuilder for DockerCliBuilder {
    fn identity(&self) -> Result<Value> {
        let server = self.run(&["version", "--format", "{{.Server.Version}}"])?;
        let buildx = self.run(&["buildx", "version"])?;
        Ok(
            json!({"docker_server": server.trim(), "buildx": buildx.trim(),
            "driver": "docker (private daemon, classic image store)"}),
        )
    }
    fn image_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .run(&["image", "ls", "--all", "--quiet", "--no-trunc"])?
            .lines()
            .map(str::to_owned)
            .collect())
    }
    fn load(&self, archive: &Path) -> Result<()> {
        self.run(&["load", "--quiet", "--input", &archive.display().to_string()])
            .map(|_| ())
    }
    fn tag(&self, image: &str, tag: &str) -> Result<()> {
        self.run(&["tag", image, tag]).map(|_| ())
    }
    fn build(
        &self,
        context: &Path,
        platform: &str,
        named_contexts: &[(String, String)],
        tag: &str,
        timeout: Duration,
    ) -> Result<BuildOutcome> {
        let mut command = self.command();
        command.args([
            "buildx",
            "build",
            "--builder",
            "default",
            "--network=none",
            "--pull=false",
            "--provenance=false",
            "--sbom=false",
            "--progress=plain",
            "--platform",
            platform,
            "--file",
        ]);
        command.arg(context.join(SELECTED_DOCKERFILE));
        for (name, target) in named_contexts {
            command
                .arg("--build-context")
                .arg(format!("{name}={target}"));
        }
        command.args(["--tag", tag, "--load"]).arg(context);
        let log = tempfile::NamedTempFile::new()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(
                log.reopen()
                    .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?,
            )
            .spawn()
            .map_err(|e| err("source_oci_builder_unavailable", e.to_string()))?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(BuildOutcome::TimedOut);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => return Err(err("source_oci_builder_unavailable", e.to_string())),
            }
        };
        let mut text = String::new();
        let _ = log.reopen().and_then(|mut f| f.read_to_string(&mut text));
        Ok(if status.success() {
            BuildOutcome::Built
        } else {
            BuildOutcome::Failed {
                log_tail: tail(&text, 4000),
            }
        })
    }
    fn image_id(&self, reference: &str) -> Result<String> {
        Ok(self
            .run(&["image", "inspect", "--format", "{{.Id}}", reference])?
            .trim()
            .to_owned())
    }
    fn save(&self, image_id: &str, output: &Path) -> Result<()> {
        // By image ID: the archive carries no repository tags.
        self.run(&["save", "--output", &output.display().to_string(), image_id])
            .map(|_| ())
    }
    fn clear(&self) -> Result<()> {
        let ids = self.image_ids()?;
        if !ids.is_empty() {
            let mut args = vec!["image", "rm", "--force"];
            args.extend(ids.iter().map(String::as_str));
            self.run(&args)?;
        }
        self.run(&["builder", "prune", "--all", "--force"])
            .map(|_| ())
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        err(
            "source_oci_input_unreadable",
            format!("{}: {e}", path.display()),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
}

fn tar_members(path: &Path, wanted: &dyn Fn(&str) -> bool) -> Result<BTreeMap<String, Vec<u8>>> {
    let file =
        std::fs::File::open(path).map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
    let mut archive = tar::Archive::new(file);
    let mut out = BTreeMap::new();
    for entry in archive
        .entries()
        .map_err(|e| err("source_oci_archive_invalid", e.to_string()))?
    {
        let mut entry = entry.map_err(|e| err("source_oci_archive_invalid", e.to_string()))?;
        let name = entry
            .path()
            .map_err(|e| err("source_oci_archive_invalid", e.to_string()))?
            .to_string_lossy()
            .into_owned();
        if entry.header().entry_type().is_file() && wanted(&name) {
            if entry.size() > 64 * 1024 * 1024 {
                return Err(err("source_oci_archive_invalid", "member exceeds bound"));
            }
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| err("source_oci_archive_invalid", e.to_string()))?;
            out.insert(name, bytes);
        }
    }
    Ok(out)
}

/// The base archive's OCI index must name exactly the pinned reference digest
/// and carry that blob. Returns the config image ID Docker assigns on load.
fn check_base_archive(base: &BaseImageInput) -> Result<String> {
    if sha256_file(&base.archive)? != base.archive_sha256 {
        return Err(err(
            "source_oci_base_digest_mismatch",
            format!(
                "{} archive bytes differ from the frozen digest",
                base.reference
            ),
        ));
    }
    let digest = base.pinned_digest.as_str();
    let blob = format!("blobs/sha256/{}", &digest[7..]);
    let members = tar_members(&base.archive, &|n| {
        n == "index.json" || n == "manifest.json" || n == blob
    })?;
    let index: Value = serde_json::from_slice(members.get("index.json").ok_or_else(|| {
        err(
            "source_oci_base_digest_mismatch",
            "base archive has no OCI index",
        )
    })?)
    .map_err(|e| err("source_oci_base_digest_mismatch", e.to_string()))?;
    let root = index["manifests"].as_array().filter(|m| m.len() == 1);
    let pinned = members
        .get(&blob)
        .is_some_and(|bytes| format!("sha256:{:x}", Sha256::digest(bytes)) == digest);
    if root.is_none_or(|m| m[0]["digest"] != digest) || !pinned {
        return Err(err(
            "source_oci_base_digest_mismatch",
            format!("{} is not the root of its frozen archive", base.reference),
        ));
    }
    let legacy: Value = serde_json::from_slice(members.get("manifest.json").ok_or_else(|| {
        err(
            "source_oci_base_digest_mismatch",
            "base archive is not docker-loadable",
        )
    })?)
    .map_err(|e| err("source_oci_base_digest_mismatch", e.to_string()))?;
    let config = legacy[0]["Config"]
        .as_str()
        .and_then(|p| p.strip_prefix("blobs/sha256/"))
        .filter(|_| legacy.as_array().is_some_and(|a| a.len() == 1))
        .ok_or_else(|| {
            err(
                "source_oci_base_digest_mismatch",
                "one loadable image required",
            )
        })?;
    Ok(format!("sha256:{config}"))
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

pub fn image_facts(archive: &Path) -> Result<ImageFacts> {
    let members = tar_members(archive, &|n| {
        n == "index.json" || n.starts_with("blobs/sha256/")
    })?;
    let invalid = |d: &str| err("source_oci_artifact_invalid", d.to_owned());
    let index: Value = serde_json::from_slice(
        members
            .get("index.json")
            .ok_or_else(|| invalid("no index"))?,
    )
    .map_err(|e| invalid(&e.to_string()))?;
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str()
        .filter(|d| is_digest(d) && index["manifests"].as_array().is_some_and(|m| m.len() == 1))
        .ok_or_else(|| invalid("index must name one manifest"))?
        .to_owned();
    let blob = |d: &str| members.get(&format!("blobs/sha256/{}", &d[7..]));
    let manifest: Value =
        serde_json::from_slice(blob(&manifest_digest).ok_or_else(|| invalid("manifest missing"))?)
            .map_err(|e| invalid(&e.to_string()))?;
    let config_digest = manifest["config"]["digest"]
        .as_str()
        .filter(|d| is_digest(d))
        .ok_or_else(|| invalid("config digest"))?
        .to_owned();
    let config: Value =
        serde_json::from_slice(blob(&config_digest).ok_or_else(|| invalid("config missing"))?)
            .map_err(|e| invalid(&e.to_string()))?;
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
        layer_digests: manifest["layers"]
            .as_array()
            .map(|l| {
                l.iter()
                    .filter_map(|x| x["digest"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
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

/// Repack a private-daemon `docker save` into the portable transport form:
/// only `oci-layout`, `index.json`, `manifest.json` and the blobs the single
/// OCI manifest references (manifest, config, layers). A classic image store
/// also writes legacy v1 per-layer JSON blobs that nothing references; the
/// portable validator refuses those. Blob bytes are content-addressed and
/// copied unchanged, so the manifest digest does not change. Returns how many
/// unreferenced members were dropped.
pub fn repack_saved_archive(saved: &Path, output: &Path) -> Result<usize> {
    let members = tar_members(saved, &|_| true)?;
    let invalid = |d: &str| err("source_oci_artifact_invalid", d.to_owned());
    let index: Value = serde_json::from_slice(
        members
            .get("index.json")
            .ok_or_else(|| invalid("no index"))?,
    )
    .map_err(|e| invalid(&e.to_string()))?;
    let manifest_digest = index["manifests"][0]["digest"]
        .as_str()
        .filter(|d| is_digest(d) && index["manifests"].as_array().is_some_and(|m| m.len() == 1))
        .ok_or_else(|| invalid("index must name one manifest"))?;
    let path = |d: &str| format!("blobs/sha256/{}", &d[7..]);
    let manifest: Value = serde_json::from_slice(
        members
            .get(&path(manifest_digest))
            .ok_or_else(|| invalid("manifest missing"))?,
    )
    .map_err(|e| invalid(&e.to_string()))?;
    let mut keep: BTreeSet<String> = ["oci-layout", "index.json", "manifest.json"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    keep.insert(path(manifest_digest));
    let config = manifest["config"]["digest"]
        .as_str()
        .filter(|d| is_digest(d))
        .ok_or_else(|| invalid("config"))?;
    keep.insert(path(config));
    for layer in manifest["layers"]
        .as_array()
        .ok_or_else(|| invalid("layers"))?
    {
        let d = layer["digest"]
            .as_str()
            .filter(|d| is_digest(d))
            .ok_or_else(|| invalid("layer"))?;
        keep.insert(path(d));
    }
    if let Some(missing) = keep.iter().find(|k| !members.contains_key(*k)) {
        return Err(invalid(&format!("referenced member {missing} missing")));
    }
    let file = std::fs::File::create(output).map_err(|e| invalid(&e.to_string()))?;
    let mut tar = tar::Builder::new(file);
    for dir in ["blobs/", "blobs/sha256/"] {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_size(0);
        header.set_cksum();
        tar.append_data(&mut header, dir, std::io::empty())
            .map_err(|e| invalid(&e.to_string()))?;
    }
    for name in &keep {
        let bytes = &members[name];
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .map_err(|e| invalid(&e.to_string()))?;
    }
    tar.into_inner().map_err(|e| invalid(&e.to_string()))?;
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

#[derive(Debug, Clone, Serialize)]
pub struct Materialized {
    pub provenance: Value,
    pub capsule_toml: String,
    pub image_reference: String,
    pub archive: PathBuf,
}

/// Build one source into one verified OCI archive plus an authored
/// `ato.capsule/2` OCI route. `out` must be a new directory.
pub fn materialize(
    request: &SourceOciRequest,
    builder: &dyn OciBuilder,
    out: &Path,
) -> Result<Materialized> {
    request.validate()?;
    std::fs::create_dir(out).map_err(|e| err("source_oci_output_invalid", e.to_string()))?;
    // 1. Frozen source closure, from digest-verified bytes only.
    let bytes = std::fs::read(&request.source_archive)
        .map_err(|e| err("source_oci_input_unreadable", e.to_string()))?;
    let limits = SourceLimits::default();
    let verified = DownloadedArchive::new(bytes)
        .verify_archive_digest(&request.source_archive_sha256)
        .and_then(|a| a.verify_tree_digest(None, limits))
        .map_err(|e| err("source_oci_source_invalid", e.to_string()))?;
    let closure = verified
        .closure_ref("")
        .map_err(|e| err("source_oci_source_invalid", e.to_string()))?
        .as_str()
        .to_owned();
    let context = verified
        .materialize(&out.join("context"), "", limits)
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
        .map(|_| sha256_file(&dockerignore))
        .transpose()?;
    // 2. Private, empty builder only.
    if !builder.image_ids()?.is_empty() {
        return Err(err(
            "source_oci_builder_store_not_empty",
            "the private builder store must be empty; no other job's images or cache",
        ));
    }
    let identity = builder.identity()?;
    // 3. Frozen external image references.
    let mut named = Vec::new();
    let mut bases = Vec::new();
    for (n, base) in request.base_images.iter().enumerate() {
        let config = check_base_archive(base)?;
        builder.load(&base.archive)?;
        let tag = format!("ato-base/b{n}:frozen");
        builder.tag(&config, &tag)?;
        named.push((base.reference.clone(), format!("docker-image://{tag}")));
        bases.push(
            json!({"reference":base.reference,"archive_sha256":base.archive_sha256,
            "pinned_digest":base.pinned_digest,"config_digest":config}),
        );
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
    let cleanup = |result: Result<Materialized>| -> Result<Materialized> {
        let cleared = builder.clear().and_then(|()| {
            if builder.image_ids()?.is_empty() {
                Ok(())
            } else {
                Err(err(
                    "source_oci_cleanup_unconfirmed",
                    "private store not empty after cleanup",
                ))
            }
        });
        match (result, cleared) {
            (Ok(m), Ok(())) => Ok(m),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
        }
    };
    match outcome {
        BuildOutcome::Built => {}
        BuildOutcome::TimedOut => {
            return cleanup(Err(err(
                "source_oci_build_timeout",
                format!("build exceeded {} s", request.policy.build_timeout_seconds),
            )));
        }
        BuildOutcome::Failed { log_tail } => {
            return cleanup(Err(err("source_oci_build_failed", log_tail)));
        }
    }
    let result = (|| -> Result<Materialized> {
        // 5. Export and verify with the existing portable OCI validator.
        let image_id = builder.image_id(tag)?;
        let saved = out.join("image.saved.tar");
        builder.save(&image_id, &saved)?;
        let archive = out.join("image.tar");
        let dropped = repack_saved_archive(&saved, &archive)?;
        std::fs::remove_file(&saved)
            .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?;
        let size = std::fs::metadata(&archive)
            .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?
            .len();
        if size > request.policy.max_archive_bytes {
            return Err(err(
                "source_oci_artifact_bounds",
                format!("{size} bytes exceeds the archive bound"),
            ));
        }
        let facts = image_facts(&archive)?;
        if facts.config_digest != image_id {
            return Err(err(
                "source_oci_artifact_digest_mismatch",
                "saved config differs from the built image",
            ));
        }
        let short = &closure[7..19];
        let image_reference = format!("ato-source/{short}@{}", facts.manifest_digest);
        let portable = PortableOciArchive {
            image: image_reference.clone(),
            platform: request.platform.clone(),
            bytes: base64::engine::general_purpose::STANDARD.encode(
                std::fs::read(&archive)
                    .map_err(|e| err("source_oci_artifact_invalid", e.to_string()))?,
            ),
        };
        let (os, arch) = request.platform.split_once('/').expect("validated");
        if facts.os != os || facts.architecture != arch {
            return Err(err(
                "source_oci_platform_mismatch",
                format!("{}/{}", facts.os, facts.architecture),
            ));
        }
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
        let provenance = json!({
            "schema": SOURCE_OCI_PROVENANCE_SCHEMA,
            "inputs": {
                "source_archive_sha256": request.source_archive_sha256,
                "source_closure_ref": closure,
                "dockerfile": SELECTED_DOCKERFILE,
                "dockerfile_sha256": sha256_file(&dockerfile)?,
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
                "archive_sha256": sha256_file(&archive)?,
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
    })();
    cleanup(result)
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string")
}

/// The Ato-owned authored route: one OCI serving step over the verified image.
/// K observes only GET / = 200 on the Surface; it is not a functional claim.
fn authored_route(request: &SourceOciRequest, image: &str, facts: &ImageFacts) -> String {
    let p = &request.policy;
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
