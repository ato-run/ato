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
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CACHE_FORMAT: &str = "ato-model-cache/1";
const CHUNK_BYTES: u64 = 256 * 1024 * 1024;
const CHUNK_RETRIES: u32 = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct GrantedModelSet {
    pub input_id: String,
    pub digest: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrantSummary {
    pub run_id: String,
    pub model_sets: Vec<GrantedModelSet>,
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
    pub bytes_transferred: u64,
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
        mut fetch: impl FnMut(u64, u64) -> Result<Box<dyn Read>>,
        mut keepalive: impl FnMut() -> Result<()>,
    ) -> Result<ObjectDelivery> {
        let started = Instant::now();
        let object = self.object_path(&entry.digest)?;
        if object.exists() {
            if self.marker_matches(entry, &object) {
                return Ok(ObjectDelivery {
                    digest: entry.digest.clone(),
                    cache: "hit",
                    bytes_transferred: 0,
                    millis: started.elapsed().as_millis(),
                });
            }
            match hash_file(&object) {
                Ok((digest, size)) if digest == entry.digest && size == entry.bytes => {
                    self.seal(entry, &object)?;
                    return Ok(ObjectDelivery {
                        digest: entry.digest.clone(),
                        cache: "rehash",
                        bytes_transferred: 0,
                        millis: started.elapsed().as_millis(),
                    });
                }
                _ => self.forget(entry, &object),
            }
        }

        let partial = object.with_extension("partial");
        let mut transferred = 0_u64;
        loop {
            let have = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
            if have > entry.bytes {
                fs::remove_file(&partial)?;
                continue;
            }
            if have == entry.bytes {
                break;
            }
            keepalive()?;
            let end = (have + CHUNK_BYTES).min(entry.bytes) - 1;
            let mut attempt = 0;
            loop {
                let result = (|| -> Result<u64> {
                    let mut reader = fetch(have, end)?;
                    let mut file = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&partial)?;
                    let copied = std::io::copy(&mut reader, &mut file)?;
                    file.flush()?;
                    ensure!(copied == end - have + 1, "short range: {copied} bytes");
                    Ok(copied)
                })();
                match result {
                    Ok(copied) => {
                        transferred += copied;
                        break;
                    }
                    Err(error) if attempt + 1 < CHUNK_RETRIES => {
                        attempt += 1;
                        // A short write leaves a prefix that is still valid;
                        // anything past `have` is discarded and refetched.
                        if let Ok(file) = OpenOptions::new().write(true).open(&partial) {
                            let _ = file.set_len(have);
                        }
                        eprintln!(
                            "[data-plane] retry {attempt} for {}: {error:#}",
                            entry.digest
                        );
                        std::thread::sleep(Duration::from_secs(2_u64.pow(attempt)));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        let (digest, size) = hash_file(&partial)?;
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
            bytes_transferred: transferred,
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
    mut keepalive: impl FnMut() -> Result<()>,
) -> Result<Option<(Vec<DeliveredInput>, DeliveryReport)>> {
    let Some(grant) = data.grant()? else {
        return Ok(None);
    };
    ensure!(grant.run_id == run_id, "the data grant names another Run");
    let started = Instant::now();
    let cache = ModelCache::open(work_root)?;
    let mut inputs = Vec::new();
    let mut objects = Vec::new();
    for set in &grant.model_sets {
        let manifest = data.manifest(&set.digest)?;
        for entry in &manifest.objects {
            let delivered = cache.ensure(
                entry,
                |start, end| Ok(Box::new(data.range(&entry.digest, start, end)?) as Box<dyn Read>),
                &mut keepalive,
            )?;
            eprintln!(
                "[data-plane] {} {} {} bytes in {} ms",
                delivered.cache, delivered.digest, delivered.bytes_transferred, delivered.millis
            );
            objects.push(delivered);
        }
        let dest = lease_root.join("inputs").join(&set.input_id);
        cache.materialize(&manifest, &dest)?;
        inputs.push(DeliveredInput {
            env_name: env_name(&set.input_id),
            path: dest,
        });
    }
    let report = DeliveryReport {
        objects,
        millis: started.elapsed().as_millis(),
    };
    Ok(Some((inputs, report)))
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
        fetched: &std::cell::Cell<u64>,
    ) -> impl FnMut(u64, u64) -> Result<Box<dyn Read>> + '_ {
        move |start, end| {
            fetched.set(fetched.get() + (end - start + 1));
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
        let fetched = std::cell::Cell::new(0);
        let first = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!(
            (first.cache, first.bytes_transferred),
            ("miss", data.len() as u64)
        );
        let second = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!((second.cache, second.bytes_transferred), ("hit", 0));
        assert_eq!(fetched.get(), data.len() as u64);
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
    fn a_corrupted_cache_entry_is_refetched_and_bad_bytes_never_enter_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"weights".to_vec();
        let e = entry(&data);
        let fetched = std::cell::Cell::new(0);
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
        let fetched = std::cell::Cell::new(0);
        let delivered = cache
            .ensure(&e, serve(data.clone(), &fetched), || Ok(()))
            .unwrap();
        assert_eq!(delivered.bytes_transferred, 600);
        assert_eq!(fetched.get(), 600);
    }

    #[test]
    fn keepalive_failure_stops_the_download() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = vec![1_u8; 10];
        let fetched = std::cell::Cell::new(0);
        assert!(
            cache
                .ensure(&entry(&data), serve(data.clone(), &fetched), || bail!(
                    "authorization lost"
                ))
                .is_err()
        );
        assert_eq!(fetched.get(), 0);
    }

    #[test]
    fn materialized_tree_is_read_only_links_and_manifests_must_hash() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"abc".to_vec();
        let e = entry(&data);
        let fetched = std::cell::Cell::new(0);
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
