//! Data Plane delivery on the Runner (ato-api `managed-data-plane.md` §4.3).
//!
//! For a Run whose lease carries a Data Grant, the Runner fetches each granted
//! Model Set's manifest, makes every listed object present in a machine-local
//! cache, and hands the workload a read-only tree of them. Rules:
//!
//! - a manifest is accepted only if its bytes hash to the granted digest;
//! - an object is downloaded with `Range` requests into `<hex>.partial`,
//!   resumed from what is already there, size-checked, fully SHA-256'd, and
//!   only then renamed into the cache;
//! - a cached object is trusted without re-hashing only if its Runner-only
//!   marker records the same digest, size, mtime and cache format; otherwise
//!   it is re-hashed, and a mismatch deletes it and fetches again;
//! - the workload sees hard links in its lease directory, read-only (0444,
//!   root-owned, and Landlock read-only); the cache and markers are not
//!   reachable from it. The cache dies with the machine; a new machine misses.
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod delivery;
mod ranges;

pub const CACHE_FORMAT: &str = "ato-model-cache/1";
const CHUNK_BYTES: u64 = 256 * 1024 * 1024;
const CHUNK_RETRIES: u32 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct GrantedModelSet {
    pub input_id: String,
    pub digest: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantedOutputs {
    pub max_count: usize,
    pub max_bytes_each: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantSummary {
    pub run_id: String,
    pub model_sets: Vec<GrantedModelSet>,
    #[serde(default)]
    pub input_assets: Vec<String>,
    pub outputs: GrantedOutputs,
}

/// What a granted Run gets from delivery.
pub struct Delivered {
    pub inputs: Vec<DeliveredInput>,
    /// Where the workload writes outputs to be saved when the Run stops.
    pub output_dir: Option<PathBuf>,
    pub outputs: GrantedOutputs,
    pub report: DeliveryReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSetManifest {
    pub schema: String,
    pub objects: Vec<ModelSetEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSetEntry {
    pub path: String,
    pub digest: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObjectDelivery {
    pub digest: String,
    /// `hit` (marker matched), `rehash` (present, re-verified), `miss` (fetched).
    pub cache: &'static str,
    /// Sum of requested byte ranges, including retries (zero for cache hits).
    pub bytes_requested: u64,
    /// Bytes actually read from responses, including partial failed transfers.
    pub bytes_transferred: u64,
    /// Marker checks and full-file SHA-256 verification, excluding download.
    pub verification_millis: u128,
    pub millis: u128,
}

#[derive(Debug, Serialize)]
pub struct DeliveryReport {
    pub objects: Vec<ObjectDelivery>,
    pub millis: u128,
}

/// A read-only input the workload receives: env var name and host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredInput {
    pub env_name: String,
    pub path: PathBuf,
}

pub fn env_name(input_id: &str) -> String {
    let id: String = input_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("ATO_INPUT_PATH_{id}")
}

fn sha256_ref(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn hex_of(digest: &str) -> Result<&str> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .with_context(|| format!("not a sha256 digest: {digest}"))?;
    Ok(hex)
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

pub fn parse_manifest(bytes: &[u8], granted_digest: &str) -> Result<ModelSetManifest> {
    ensure!(
        sha256_ref(bytes) == granted_digest,
        "Model Set manifest does not hash to the granted digest"
    );
    let manifest: ModelSetManifest =
        serde_json::from_slice(bytes).context("Model Set manifest is malformed")?;
    ensure!(
        manifest.schema == "ato.model-set/1",
        "unsupported Model Set schema"
    );
    ensure!(!manifest.objects.is_empty(), "empty Model Set");
    for entry in &manifest.objects {
        hex_of(&entry.digest)?;
        ensure!(
            safe_relative(&entry.path) && entry.bytes > 0,
            "invalid Model Set entry"
        );
    }
    Ok(manifest)
}

/// Fetches through the lease's data endpoints with the Runner's own token.
pub struct LeaseData<'a> {
    pub client: &'a Client,
    pub base: &'a str,
    pub token: &'a str,
    pub lease_id: &'a str,
}

impl LeaseData<'_> {
    fn url(&self, tail: &str) -> String {
        format!(
            "{}/v1/runner-leases/{}/data{tail}",
            self.base.trim_end_matches('/'),
            self.lease_id
        )
    }

    /// `None` when this lease has no grant (the Run uses no data plane).
    pub fn grant(&self) -> Result<Option<GrantSummary>> {
        let response = self
            .client
            .get(self.url(""))
            .bearer_auth(self.token)
            .send()
            .context("data grant request failed")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "data grant refused: {}",
            response.status()
        );
        Ok(Some(response.json().context("data grant is malformed")?))
    }

    pub fn manifest(&self, digest: &str) -> Result<ModelSetManifest> {
        let response = self
            .client
            .get(self.url(&format!("/model-sets/{digest}")))
            .bearer_auth(self.token)
            .send()
            .context("Model Set manifest request failed")?;
        ensure!(
            response.status().is_success(),
            "Model Set manifest refused: {}",
            response.status()
        );
        let bytes = response.bytes().context("Model Set manifest read failed")?;
        parse_manifest(&bytes, digest)
    }

    fn range(
        &self,
        digest: &str,
        start: u64,
        end_inclusive: u64,
    ) -> Result<reqwest::blocking::Response> {
        let response = self
            .client
            .get(self.url(&format!("/model-objects/{digest}")))
            .bearer_auth(self.token)
            .header("range", format!("bytes={start}-{end_inclusive}"))
            .timeout(Duration::from_secs(1800))
            .send()
            .context("model object request failed")?;
        ensure!(
            response.status() == reqwest::StatusCode::PARTIAL_CONTENT,
            "model object range refused: {}",
            response.status()
        );
        Ok(response)
    }

    /// Download one granted input Asset to `dest/<asset_id>/<filename>`,
    /// verified against the SHA-256 the Coordinator recorded for it.
    pub fn asset(&self, asset_id: &str, dest: &Path) -> Result<PathBuf> {
        ensure!(
            asset_id.starts_with("ast_")
                && asset_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid Asset id"
        );
        let mut response = self
            .client
            .get(self.url(&format!("/assets/{asset_id}")))
            .bearer_auth(self.token)
            .timeout(Duration::from_secs(1800))
            .send()
            .context("input Asset request failed")?;
        ensure!(
            response.status().is_success(),
            "input Asset refused: {}",
            response.status()
        );
        let expected = response
            .headers()
            .get("x-ato-asset-sha256")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .context("input Asset carries no digest")?;
        let filename = response
            .headers()
            .get("x-ato-asset-filename")
            .and_then(|v| v.to_str().ok())
            .and_then(percent_decode)
            .filter(|name| safe_relative(name) && !name.contains('/'))
            .unwrap_or_else(|| "asset".to_owned());
        let dir = dest.join(asset_id);
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
        let path = dir.join(filename);
        let partial = path.with_extension("partial");
        {
            let mut file = File::create(&partial)?;
            std::io::copy(&mut response, &mut file)?;
            file.flush()?;
        }
        let (digest, _) = hash_file(&partial)?;
        if digest != expected {
            let _ = fs::remove_file(&partial);
            bail!("input Asset {asset_id} failed verification");
        }
        fs::set_permissions(&partial, fs::Permissions::from_mode(0o444))?;
        fs::rename(&partial, &path)?;
        Ok(path)
    }

    fn post_json(&self, tail: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        let response = self
            .client
            .post(self.url(tail))
            .bearer_auth(self.token)
            .json(body)
            .send()
            .with_context(|| format!("{tail} failed"))?;
        let status = response.status();
        let value: serde_json::Value = response.json().unwrap_or(serde_json::Value::Null);
        ensure!(status.is_success(), "{tail} refused: {status} {value}");
        Ok(value)
    }

    pub fn report(&self, report: &DeliveryReport) -> Result<()> {
        let response = self
            .client
            .post(self.url("/delivery-report"))
            .bearer_auth(self.token)
            .json(report)
            .send()
            .context("delivery report failed")?;
        ensure!(
            response.status().is_success(),
            "delivery report refused: {}",
            response.status()
        );
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Marker {
    format: String,
    digest: String,
    bytes: u64,
    mtime_ns: u128,
}

pub struct ModelCache {
    objects: PathBuf,
    markers: PathBuf,
}

fn mtime_ns(path: &Path) -> Result<u128> {
    Ok(fs::metadata(path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 8 * 1024 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((format!("sha256:{}", hex::encode(hasher.finalize())), total))
}

struct CountedRead<'a> {
    reader: Box<dyn Read>,
    transferred: &'a mut u64,
}

impl Read for CountedRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.reader.read(buffer)?;
        *self.transferred += count as u64;
        Ok(count)
    }
}

impl ModelCache {
    /// Under the Runner's work root, outside every lease directory.
    pub fn open(work_root: &Path) -> Result<Self> {
        let root = work_root.join("model-cache");
        let cache = Self {
            objects: root.join("objects"),
            markers: root.join("markers"),
        };
        for dir in [&root, &cache.objects, &cache.markers] {
            fs::create_dir_all(dir)?;
        }
        // Markers are the Runner's alone.
        fs::set_permissions(&cache.markers, fs::Permissions::from_mode(0o700))?;
        Ok(cache)
    }

    pub fn object_path(&self, digest: &str) -> Result<PathBuf> {
        Ok(self.objects.join(hex_of(digest)?))
    }

    fn marker_path(&self, digest: &str) -> Result<PathBuf> {
        Ok(self.markers.join(format!("{}.json", hex_of(digest)?)))
    }

    fn marker_matches(&self, entry: &ModelSetEntry, object: &Path) -> bool {
        let Ok(text) = fs::read(self.marker_path(&entry.digest).unwrap_or_default()) else {
            return false;
        };
        let Ok(marker) = serde_json::from_slice::<Marker>(&text) else {
            return false;
        };
        let Ok(meta) = fs::metadata(object) else {
            return false;
        };
        marker
            == Marker {
                format: CACHE_FORMAT.to_owned(),
                digest: entry.digest.clone(),
                bytes: entry.bytes,
                mtime_ns: mtime_ns(object).unwrap_or_default(),
            }
            && meta.len() == entry.bytes
    }

    fn seal(&self, entry: &ModelSetEntry, object: &Path) -> Result<()> {
        fs::set_permissions(object, fs::Permissions::from_mode(0o444))?;
        let marker = Marker {
            format: CACHE_FORMAT.to_owned(),
            digest: entry.digest.clone(),
            bytes: entry.bytes,
            mtime_ns: mtime_ns(object)?,
        };
        let path = self.marker_path(&entry.digest)?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(&marker)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    fn forget(&self, entry: &ModelSetEntry, object: &Path) {
        let _ = fs::remove_file(object);
        if let Ok(marker) = self.marker_path(&entry.digest) {
            let _ = fs::remove_file(marker);
        }
    }

    /// Make one object present and verified. `fetch(start, end)` returns a
    /// reader for that inclusive byte range; `keepalive` is called between
    /// chunks and aborts the download by returning an error.
    pub fn ensure(
        &self,
        entry: &ModelSetEntry,
        fetch: impl Fn(u64, u64) -> Result<Box<dyn Read>> + Sync,
        keepalive: impl FnMut() -> Result<()> + Send,
    ) -> Result<ObjectDelivery> {
        let started = Instant::now();
        let mut verification = Duration::ZERO;
        let object = self.object_path(&entry.digest)?;
        if object.exists() {
            let verifying = Instant::now();
            let marker_matches = self.marker_matches(entry, &object);
            verification += verifying.elapsed();
            if marker_matches {
                return Ok(ObjectDelivery {
                    digest: entry.digest.clone(),
                    cache: "hit",
                    bytes_requested: 0,
                    bytes_transferred: 0,
                    verification_millis: verification.as_millis(),
                    millis: started.elapsed().as_millis(),
                });
            }
            let verifying = Instant::now();
            let checked = hash_file(&object);
            verification += verifying.elapsed();
            match checked {
                Ok((digest, size)) if digest == entry.digest && size == entry.bytes => {
                    self.seal(entry, &object)?;
                    return Ok(ObjectDelivery {
                        digest: entry.digest.clone(),
                        cache: "rehash",
                        bytes_requested: 0,
                        bytes_transferred: 0,
                        verification_millis: verification.as_millis(),
                        millis: started.elapsed().as_millis(),
                    });
                }
                _ => self.forget(entry, &object),
            }
        }

        let partial = object.with_extension("partial");
        let (requested, transferred) =
            ranges::hydrate(&partial, entry.bytes, CHUNK_BYTES, fetch, keepalive)?;
        let verifying = Instant::now();
        let (digest, size) = hash_file(&partial)?;
        verification += verifying.elapsed();
        if digest != entry.digest || size != entry.bytes {
            let _ = fs::remove_file(&partial);
            bail!(
                "model object {} failed verification ({digest}, {size} bytes)",
                entry.digest
            );
        }
        fs::rename(&partial, &object)?;
        self.seal(entry, &object)?;
        Ok(ObjectDelivery {
            digest: entry.digest.clone(),
            cache: "miss",
            bytes_requested: requested,
            bytes_transferred: transferred,
            verification_millis: verification.as_millis(),
            millis: started.elapsed().as_millis(),
        })
    }

    /// Hard-link a verified Model Set into `dest` as `dest/<path>`, read-only.
    pub fn materialize(&self, manifest: &ModelSetManifest, dest: &Path) -> Result<()> {
        fs::create_dir_all(dest)?;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
        for entry in &manifest.objects {
            let target = dest.join(&entry.path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                let mut dir = parent.to_path_buf();
                while dir.starts_with(dest) {
                    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
                    if !dir.pop() {
                        break;
                    }
                }
            }
            let _ = fs::remove_file(&target);
            fs::hard_link(self.object_path(&entry.digest)?, &target).with_context(|| {
                format!(
                    "cannot link {} into the Run (cache and work root must share a filesystem)",
                    entry.path
                )
            })?;
        }
        Ok(())
    }
}

/// Deliver every granted Model Set for this lease. Returns the read-only
/// inputs for the workload and the delivery report, or `None` when the lease
/// has no grant.
pub fn deliver(
    data: &LeaseData<'_>,
    run_id: &str,
    work_root: &Path,
    lease_root: &Path,
    mut keepalive: impl FnMut() -> Result<()> + Send,
) -> Result<Option<Delivered>> {
    let Some(grant) = data.grant()? else {
        return Ok(None);
    };
    ensure!(grant.run_id == run_id, "the data grant names another Run");
    let mut input_names = std::collections::BTreeSet::new();
    for set in &grant.model_sets {
        ensure!(
            safe_relative(&set.input_id)
                && !set.input_id.contains('/')
                && set.input_id != "assets"
                && input_names.insert(env_name(&set.input_id)),
            "data grant has an unsafe or colliding input name"
        );
    }
    let started = Instant::now();
    let cache = ModelCache::open(work_root)?;
    let mut inputs = Vec::new();
    let mut manifests = Vec::new();
    for set in &grant.model_sets {
        manifests.push(data.manifest(&set.digest)?);
    }
    let entries: Vec<_> = manifests.iter().flat_map(|m| m.objects.iter()).collect();
    let objects = delivery::ensure_objects(
        &cache,
        &entries,
        |entry, start, end| Ok(Box::new(data.range(&entry.digest, start, end)?) as Box<dyn Read>),
        &mut keepalive,
    )?;
    for (set, manifest) in grant.model_sets.iter().zip(&manifests) {
        let dest = lease_root.join("inputs").join(&set.input_id);
        cache.materialize(manifest, &dest)?;
        inputs.push(DeliveredInput {
            env_name: env_name(&set.input_id),
            path: dest,
        });
    }
    if !grant.input_assets.is_empty() {
        let dest = lease_root.join("inputs").join("assets");
        fs::create_dir_all(&dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
        for asset_id in &grant.input_assets {
            keepalive()?;
            data.asset(asset_id, &dest)?;
        }
        inputs.push(DeliveredInput {
            env_name: "ATO_INPUT_ASSETS_DIR".to_owned(),
            path: dest,
        });
    }
    let output_dir = (grant.outputs.max_count > 0).then(|| lease_root.join("outputs"));
    let report = DeliveryReport {
        objects,
        millis: started.elapsed().as_millis(),
    };
    Ok(Some(Delivered {
        inputs,
        output_dir,
        outputs: grant.outputs,
        report,
    }))
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One saved (or not) output, as reported to the log and the Coordinator.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct OutputSave {
    pub output_key: String,
    pub save_status: &'static str,
    pub asset_id: Option<String>,
    pub error: Option<String>,
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("json") => "application/json",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Every regular file under `root` (no symlinks followed), as relative keys.
fn output_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let key = entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push((key, entry.path()));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Save what the workload left in its output directory, after the Run has
/// stopped (the files are final). Each file is recorded as generated (size,
/// SHA-256, mtime) before any byte is uploaded; a failure after that is
/// reported as save_failed, and the generation stays recorded.
pub fn save_outputs(data: &LeaseData<'_>, dir: &Path, limits: &GrantedOutputs) -> Vec<OutputSave> {
    let files = match output_files(dir) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("[data-plane] cannot list outputs: {error:#}");
            return Vec::new();
        }
    };
    files
        .into_iter()
        .take(limits.max_count)
        .map(|(key, path)| save_one(data, &key, &path, limits.max_bytes_each))
        .collect()
}

fn save_one(data: &LeaseData<'_>, key: &str, path: &Path, max_bytes: u64) -> OutputSave {
    let failed = |error: String| OutputSave {
        output_key: key.to_owned(),
        save_status: "save_failed",
        asset_id: None,
        error: Some(error),
    };
    let (digest, size) = match hash_file(path) {
        Ok(found) => found,
        Err(error) => return failed(format!("cannot read the output: {error:#}")),
    };
    if size == 0 || size > max_bytes {
        return failed(format!(
            "{size} bytes is outside the Run's output limit ({max_bytes})"
        ));
    }
    let generated_at = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| {
            time::OffsetDateTime::from(t)
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| key.to_owned());
    let reserved = match data.post_json(
        "/outputs",
        &serde_json::json!({
            "output_key": key,
            "filename": filename,
            "content_type": content_type_for(path),
            "byte_size": size,
            "checksum_sha256": digest,
            "generated_at": generated_at,
        }),
    ) {
        Ok(value) => value,
        Err(error) => return failed(format!("{error:#}")),
    };
    let output_id = reserved["output_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let part_bytes = reserved["part_bytes"].as_u64().unwrap_or(64 * 1024 * 1024);
    let upload = (|| -> Result<String> {
        let mut file = File::open(path)?;
        let mut parts = Vec::new();
        let mut number = 1_u64;
        loop {
            let mut chunk = Vec::with_capacity(part_bytes as usize);
            (&mut file).take(part_bytes).read_to_end(&mut chunk)?;
            if chunk.is_empty() {
                break;
            }
            let response = data
                .client
                .put(data.url(&format!("/outputs/{output_id}/parts/{number}")))
                .bearer_auth(data.token)
                .header("x-ato-part-sha256", sha256_ref(&chunk))
                .body(chunk)
                .timeout(Duration::from_secs(1800))
                .send()?;
            ensure!(
                response.status().is_success(),
                "part {number} refused: {}",
                response.status()
            );
            parts.push(response.json::<serde_json::Value>()?);
            number += 1;
        }
        let done = data.post_json(
            &format!("/outputs/{output_id}/complete"),
            &serde_json::json!({ "parts": parts }),
        )?;
        Ok(done["asset_id"].as_str().unwrap_or_default().to_owned())
    })();
    match upload {
        Ok(asset_id) => OutputSave {
            output_key: key.to_owned(),
            save_status: "saved",
            asset_id: Some(asset_id),
            error: None,
        },
        Err(error) => {
            let message = format!("{error:#}");
            let _ = data.post_json(
                &format!("/outputs/{output_id}/failed"),
                &serde_json::json!({ "error": message.chars().take(900).collect::<String>() }),
            );
            failed(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn entry(data: &[u8]) -> ModelSetEntry {
        ModelSetEntry {
            path: "probe/a.bin".to_owned(),
            digest: sha256_ref(data),
            bytes: data.len() as u64,
        }
    }

    fn serve(
        data: Vec<u8>,
        fetched: &std::sync::atomic::AtomicU64,
    ) -> impl Fn(u64, u64) -> Result<Box<dyn Read>> + Sync + '_ {
        move |start, end| {
            fetched.fetch_add(end - start + 1, std::sync::atomic::Ordering::Relaxed);
            Ok(
                Box::new(Cursor::new(data[start as usize..=end as usize].to_vec()))
                    as Box<dyn Read>,
            )
        }
    }

    #[test]
    fn miss_then_hit_then_rehash_after_the_marker_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data: Vec<u8> = (0..300_000_u32).map(|i| (i % 251) as u8).collect();
        let e = entry(&data);
        let fetched = std::sync::atomic::AtomicU64::new(0);
        let first = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!(
            (first.cache, first.bytes_transferred),
            ("miss", data.len() as u64)
        );
        assert_eq!(first.bytes_requested, data.len() as u64);
        assert!(first.verification_millis <= first.millis);
        let second = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!((second.cache, second.bytes_transferred), ("hit", 0));
        assert_eq!(second.bytes_requested, 0);
        assert_eq!(
            fetched.load(std::sync::atomic::Ordering::Relaxed),
            data.len() as u64
        );
        fs::remove_file(cache.marker_path(&e.digest).unwrap()).unwrap();
        assert_eq!(
            cache
                .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
                .unwrap()
                .cache,
            "rehash"
        );
        assert_eq!(
            fs::metadata(cache.object_path(&e.digest).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
    }

    #[test]
    fn retry_counts_the_short_response_as_transferred_bytes() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"verified-object";
        let requests = std::sync::atomic::AtomicU64::new(0);
        let report = cache
            .ensure(
                &entry(data),
                |_, _| {
                    let request = requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let bytes = if request == 0 {
                        data[..3].to_vec()
                    } else {
                        data.to_vec()
                    };
                    Ok(Box::new(Cursor::new(bytes)))
                },
                || Ok(()),
            )
            .unwrap();
        assert_eq!(report.bytes_requested, data.len() as u64 * 2);
        assert_eq!(report.bytes_transferred, data.len() as u64 + 3);
        assert_eq!(
            fs::read(cache.object_path(&entry(data).digest).unwrap()).unwrap(),
            data
        );
    }

    #[test]
    fn a_corrupted_cache_entry_is_refetched_and_bad_bytes_never_enter_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"weights".to_vec();
        let e = entry(&data);
        let fetched = std::sync::atomic::AtomicU64::new(0);
        cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        let object = cache.object_path(&e.digest).unwrap();
        fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&object, b"tampere").unwrap();
        let again = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!(again.cache, "miss");
        assert_eq!(fs::read(&object).unwrap(), data);
        // A source serving other bytes never produces a cache entry.
        let other = ModelSetEntry {
            path: "x".into(),
            digest: sha256_ref(b"other!!"),
            bytes: 7,
        };
        assert!(
            cache
                .ensure(&other, serve(b"wrongxx".to_vec(), &fetched), || Ok(()))
                .is_err()
        );
        assert!(!cache.object_path(&other.digest).unwrap().exists());
        assert!(
            !cache
                .object_path(&other.digest)
                .unwrap()
                .with_extension("partial")
                .exists()
        );
    }

    #[test]
    fn a_partial_download_resumes_from_what_is_there() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data: Vec<u8> = (0..1000_u32).map(|i| i as u8).collect();
        let e = entry(&data);
        let partial = cache
            .object_path(&e.digest)
            .unwrap()
            .with_extension("partial");
        fs::write(&partial, &data[..400]).unwrap();
        let fetched = std::sync::atomic::AtomicU64::new(0);
        let delivered = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!(delivered.bytes_transferred, 600);
        assert_eq!(fetched.load(std::sync::atomic::Ordering::Relaxed), 600);
    }

    #[test]
    fn keepalive_failure_stops_the_download() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = vec![1_u8; 10];
        let fetched = std::sync::atomic::AtomicU64::new(0);
        assert!(
            cache
                .ensure(&entry(&data), serve(data.clone(), &fetched), || bail!(
                    "authorization lost"
                ))
                .is_err()
        );
        assert_eq!(fetched.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn outputs_are_regular_files_only_and_symlinks_are_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"runner credential").unwrap();
        fs::create_dir_all(root.path().join("videos")).unwrap();
        fs::write(root.path().join("videos/out.mp4"), b"mp4").unwrap();
        fs::write(root.path().join("result.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.path().join("leak"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("leakdir")).unwrap();
        let files = output_files(root.path()).unwrap();
        let keys: Vec<_> = files.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["result.txt", "videos/out.mp4"]);
        assert_eq!(content_type_for(Path::new("a.MP4")), "video/mp4");
        assert_eq!(
            content_type_for(Path::new("a.bin")),
            "application/octet-stream"
        );
        assert_eq!(
            percent_decode("ref%20image.png").as_deref(),
            Some("ref image.png")
        );
        assert_eq!(percent_decode("bad%zz"), None);
    }

    #[test]
    fn materialized_tree_is_read_only_links_and_manifests_must_hash() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"abc".to_vec();
        let e = entry(&data);
        let fetched = std::sync::atomic::AtomicU64::new(0);
        cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        let manifest = ModelSetManifest {
            schema: "ato.model-set/1".into(),
            objects: vec![e.clone()],
        };
        let dest = root.path().join("leases/L1/inputs/models");
        cache.materialize(&manifest, &dest).unwrap();
        let linked = dest.join("probe/a.bin");
        assert_eq!(fs::read(&linked).unwrap(), data);
        assert_eq!(
            fs::metadata(&linked).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert_eq!(env_name("models"), "ATO_INPUT_PATH_MODELS");
        assert_eq!(env_name("wan-2.2"), "ATO_INPUT_PATH_WAN_2_2");

        let bytes = br#"{"objects":[{"bytes":3,"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","path":"a"}],"schema":"ato.model-set/1"}"#;
        assert!(parse_manifest(bytes, &sha256_ref(bytes)).is_ok());
        assert!(parse_manifest(bytes, &sha256_ref(b"other")).is_err());
        let escape = br#"{"objects":[{"bytes":3,"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","path":"../a"}],"schema":"ato.model-set/1"}"#;
        assert!(parse_manifest(escape, &sha256_ref(escape)).is_err());
    }
}
