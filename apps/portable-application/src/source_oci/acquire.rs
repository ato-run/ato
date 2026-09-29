//! Base-image acquisition, a phase separate from any build. Ato itself
//! resolves one docker.io reference to its root digest once, selects one
//! platform, downloads that manifest, config and layers through an egress
//! gate whose allowlist is exactly the registry, auth and blob endpoints, and
//! writes a frozen archive that `verify_base_archive` accepts. Redirects go
//! only to listed hosts over HTTPS; anything else stops the phase. Nothing
//! from this phase is visible to a Dockerfile's RUN steps.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::egress::{EgressAllowance, EgressGate, EgressReport};
use super::{Result, err};
use crate::oci_archive::{platform_descriptor, verify_base_archive};

/// The endpoints docker.io serves registry, token and blob requests from.
pub const DOCKER_HUB_ENDPOINTS: &[&str] = &[
    "registry-1.docker.io",
    "auth.docker.io",
    "production.cloudflare.docker.com",
];
pub const MAX_ACQUISITION_BYTES: u64 = 200 * 1024 * 1024;
const MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";

/// `docker.io/<namespace>/<name>:<tag>` (a bare `name:tag` is `library/`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HubReference {
    pub repository: String,
    pub tag: String,
}

pub fn parse_hub_reference(reference: &str) -> Result<HubReference> {
    let bad = || {
        err(
            "source_oci_base_reference_invalid",
            format!("{reference:?} is not a docker.io name:tag reference"),
        )
    };
    let rest = reference.strip_prefix("docker.io/").unwrap_or(reference);
    let (name, tag) = rest.rsplit_once(':').ok_or_else(bad)?;
    let name = if name.contains('/') {
        name.to_owned()
    } else {
        format!("library/{name}")
    };
    let valid = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_./".contains(&b))
            && !s.contains("..")
    };
    if !valid(&name)
        || name.starts_with('/')
        || name.split('/').count() != 2
        || tag.is_empty()
        || tag.len() > 128
        || !tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || reference.contains('@')
    {
        return Err(bad());
    }
    Ok(HubReference {
        repository: name,
        tag: tag.to_owned(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct AcquiredBase {
    pub reference: String,
    pub repository: String,
    pub tag: String,
    pub platform: String,
    pub root_digest: String,
    pub platform_manifest_digest: String,
    pub config_digest: String,
    pub layer_digests: Vec<String>,
    pub content_bytes: u64,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub egress: EgressReport,
}

fn sha(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

struct Fetcher {
    client: reqwest::blocking::Client,
    token: Option<String>,
    repository: String,
    content: u64,
    max: u64,
}

/// The whole cause chain: a refused redirect names its target only in a
/// nested source error.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn failed(detail: impl Into<String>) -> super::SourceOciError {
    err("source_oci_base_acquisition_failed", detail)
}

impl Fetcher {
    fn url(&self, kind: &str, reference: &str) -> String {
        format!(
            "https://registry-1.docker.io/v2/{}/{kind}/{reference}",
            self.repository
        )
    }

    fn get(&mut self, url: &str, accept: Option<&str>) -> Result<reqwest::blocking::Response> {
        for _ in 0..2 {
            let mut request = self.client.get(url);
            if let Some(accept) = accept {
                request = request.header("accept", accept);
            }
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = request
                .send()
                .map_err(|e| failed(format!("{url}: {}", error_chain(&e))))?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && self.token.is_none() {
                let challenge = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| failed("registry sent no auth challenge"))?
                    .to_owned();
                self.token = Some(self.anonymous_token(&challenge)?);
                continue;
            }
            if !response.status().is_success() {
                return Err(failed(format!("{url}: HTTP {}", response.status())));
            }
            return Ok(response);
        }
        Err(failed(format!("{url}: unauthorized")))
    }

    /// Anonymous pull token from the realm the registry names; the realm
    /// must itself be an allowlisted HTTPS endpoint.
    fn anonymous_token(&mut self, challenge: &str) -> Result<String> {
        let params = challenge
            .strip_prefix("Bearer ")
            .ok_or_else(|| failed("unsupported auth challenge"))?
            .split(',')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().trim_matches('"').to_owned()))
            .collect::<BTreeMap<_, _>>();
        let realm = params.get("realm").ok_or_else(|| failed("no auth realm"))?;
        let host = reqwest::Url::parse(realm)
            .ok()
            .filter(|u| u.scheme() == "https")
            .and_then(|u| u.host_str().map(str::to_owned))
            .ok_or_else(|| failed("auth realm is not an HTTPS URL"))?;
        if !DOCKER_HUB_ENDPOINTS.contains(&host.as_str()) {
            return Err(failed(format!("auth realm host {host} is not allowlisted")));
        }
        let response = self
            .client
            .get(realm)
            .query(&[
                (
                    "service",
                    params.get("service").cloned().unwrap_or_default(),
                ),
                ("scope", params.get("scope").cloned().unwrap_or_default()),
            ])
            .send()
            .map_err(|e| failed(format!("token: {e}")))?;
        let body: Value = serde_json::from_slice(&self.read(response, MANIFEST_BYTES)?)
            .map_err(|e| failed(format!("token: {e}")))?;
        body["token"]
            .as_str()
            .or_else(|| body["access_token"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| failed("token response has no token"))
    }

    /// Read a bounded body, counting it against the phase's content bound.
    fn read(&mut self, response: reqwest::blocking::Response, max: u64) -> Result<Vec<u8>> {
        let mut body = Vec::new();
        response
            .take(max + 1)
            .read_to_end(&mut body)
            .map_err(|e| failed(e.to_string()))?;
        self.count(body.len() as u64)?;
        if body.len() as u64 > max {
            return Err(failed("response exceeds its bound"));
        }
        Ok(body)
    }

    fn count(&mut self, bytes: u64) -> Result<()> {
        self.content += bytes;
        if self.content > self.max {
            return Err(err(
                "source_oci_base_acquisition_bound",
                format!("base acquisition exceeded {} bytes", self.max),
            ));
        }
        Ok(())
    }

    /// A manifest or index by tag or digest; its digest is the hash of the
    /// bytes received, and a digest reference must equal it.
    fn manifest(&mut self, reference: &str) -> Result<(String, Vec<u8>, Value)> {
        let url = self.url("manifests", reference);
        let response = self.get(&url, Some(ACCEPT))?;
        let header = response
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = self.read(response, MANIFEST_BYTES)?;
        let digest = sha(&body);
        if reference.starts_with("sha256:") && reference != digest
            || header.as_deref().is_some_and(|h| h != digest)
        {
            return Err(failed(format!("{reference}: manifest digest mismatch")));
        }
        let value = serde_json::from_slice(&body).map_err(|e| failed(e.to_string()))?;
        Ok((digest, body, value))
    }

    /// A blob streamed to `path`, verified against its descriptor.
    fn blob(&mut self, descriptor: &Value, path: &Path) -> Result<(String, u64)> {
        let digest = descriptor["digest"]
            .as_str()
            .filter(|d| super::is_digest(d))
            .ok_or_else(|| failed("descriptor digest"))?
            .to_owned();
        let size = descriptor["size"]
            .as_u64()
            .ok_or_else(|| failed("descriptor size"))?;
        self.count(size)?;
        let url = self.url("blobs", &digest);
        let mut response = self.get(&url, None)?;
        let mut file = BufWriter::new(File::create(path).map_err(|e| failed(e.to_string()))?);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut read = 0_u64;
        loop {
            let n = response
                .read(&mut buffer)
                .map_err(|e| failed(e.to_string()))?;
            if n == 0 {
                break;
            }
            read += n as u64;
            if read > size {
                return Err(failed(format!("{digest}: larger than its descriptor")));
            }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n])
                .map_err(|e| failed(e.to_string()))?;
        }
        file.flush().map_err(|e| failed(e.to_string()))?;
        if read != size || format!("sha256:{:x}", hasher.finalize()) != digest {
            return Err(failed(format!("{digest}: digest/size mismatch")));
        }
        Ok((digest, size))
    }
}

fn append(
    tar: &mut tar::Builder<BufWriter<File>>,
    name: &str,
    reader: impl Read,
    size: u64,
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_size(size);
    header.set_cksum();
    tar.append_data(&mut header, name, reader)
        .map_err(|e| failed(e.to_string()))
}

/// Acquire `reference` for `platform` into `archive` (a new file). `scratch`
/// is an empty directory for blobs in transit.
pub fn acquire_base(
    reference: &str,
    platform: &str,
    max_bytes: u64,
    archive: &Path,
    scratch: &Path,
) -> Result<AcquiredBase> {
    let hub = parse_hub_reference(reference)?;
    if !matches!(platform, "linux/amd64" | "linux/arm64") {
        return Err(failed("platform must be linux/amd64 or linux/arm64"));
    }
    if !(1..=MAX_ACQUISITION_BYTES).contains(&max_bytes) {
        return Err(failed(format!(
            "acquisition bound must be 1..={MAX_ACQUISITION_BYTES}"
        )));
    }
    let gate = EgressGate::start(
        "127.0.0.1:0".parse().expect("loopback"),
        EgressAllowance {
            hosts: DOCKER_HUB_ENDPOINTS
                .iter()
                .map(|h| (*h).to_owned())
                .collect(),
            ports: vec![443],
            max_transfer_bytes: max_bytes,
        },
    )?;
    let proxy = format!("http://{}", gate.address());
    let result = (|| {
        let client = reqwest::blocking::Client::builder()
            .proxy(reqwest::Proxy::all(&proxy).map_err(|e| failed(e.to_string()))?)
            .https_only(true)
            .timeout(std::time::Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let listed = attempt.url().scheme() == "https"
                    && attempt
                        .url()
                        .host_str()
                        .is_some_and(|h| DOCKER_HUB_ENDPOINTS.contains(&h));
                if attempt.previous().len() > 3 {
                    attempt.error("too many redirects")
                } else if listed {
                    attempt.follow()
                } else {
                    let target = attempt.url().to_string();
                    attempt.error(format!("redirect to an unlisted endpoint: {target}"))
                }
            }))
            .build()
            .map_err(|e| failed(e.to_string()))?;
        let mut fetcher = Fetcher {
            client,
            token: None,
            repository: hub.repository.clone(),
            content: 0,
            max: max_bytes,
        };
        let (root, root_bytes, root_json) = fetcher.manifest(&hub.tag)?;
        let (platform_digest, platform_bytes, manifest) = if root_json.get("manifests").is_some() {
            let descriptor =
                platform_descriptor(&root_json, platform).map_err(|e| failed(format!("{e:#}")))?;
            let digest = descriptor["digest"]
                .as_str()
                .ok_or_else(|| failed("platform descriptor digest"))?
                .to_owned();
            let (d, bytes, json) = fetcher.manifest(&digest)?;
            (d, Some(bytes), json)
        } else {
            (root.clone(), None, root_json.clone())
        };
        let mut blobs = Vec::new();
        let config_path = scratch.join("config");
        let (config, config_size) = fetcher.blob(&manifest["config"], &config_path)?;
        blobs.push((config.clone(), config_path, config_size));
        let mut layers = Vec::new();
        for (n, layer) in manifest["layers"]
            .as_array()
            .ok_or_else(|| failed("manifest has no layers"))?
            .iter()
            .enumerate()
        {
            let path = scratch.join(format!("layer-{n}"));
            let (digest, size) = fetcher.blob(layer, &path)?;
            layers.push(digest.clone());
            blobs.push((digest, path, size));
        }
        // OCI layout rooted at the resolved root, plus Docker's load manifest.
        let file = File::create(archive).map_err(|e| failed(e.to_string()))?;
        let mut tar = tar::Builder::new(BufWriter::new(file));
        let blob_name = |d: &str| format!("blobs/sha256/{}", &d[7..]);
        let layout = br#"{"imageLayoutVersion":"1.0.0"}"#;
        append(&mut tar, "oci-layout", &layout[..], layout.len() as u64)?;
        let media_type = root_json["mediaType"]
            .as_str()
            .unwrap_or(if root_json.get("manifests").is_some() {
                "application/vnd.oci.image.index.v1+json"
            } else {
                "application/vnd.oci.image.manifest.v1+json"
            })
            .to_owned();
        let index = serde_json::to_vec(&serde_json::json!({"schemaVersion":2,
            "manifests":[{"mediaType":media_type,"digest":root,"size":root_bytes.len()}]}))
        .map_err(|e| failed(e.to_string()))?;
        append(&mut tar, "index.json", index.as_slice(), index.len() as u64)?;
        let legacy = serde_json::to_vec(&serde_json::json!([{
            "Config": blob_name(&config),
            "RepoTags": null,
            "Layers": layers.iter().map(|l| blob_name(l)).collect::<Vec<_>>(),
        }]))
        .map_err(|e| failed(e.to_string()))?;
        append(
            &mut tar,
            "manifest.json",
            legacy.as_slice(),
            legacy.len() as u64,
        )?;
        append(
            &mut tar,
            &blob_name(&root),
            root_bytes.as_slice(),
            root_bytes.len() as u64,
        )?;
        if let Some(bytes) = &platform_bytes {
            append(
                &mut tar,
                &blob_name(&platform_digest),
                bytes.as_slice(),
                bytes.len() as u64,
            )?;
        }
        for (digest, path, size) in &blobs {
            let reader = File::open(path).map_err(|e| failed(e.to_string()))?;
            append(&mut tar, &blob_name(digest), reader, *size)?;
        }
        tar.into_inner()
            .map_err(|e| failed(e.to_string()))?
            .flush()
            .map_err(|e| failed(e.to_string()))?;
        // The frozen archive must pass the same check a build applies.
        let graph = verify_base_archive(
            File::open(archive).map_err(|e| failed(e.to_string()))?,
            &root,
            platform,
        )
        .map_err(|e| failed(format!("acquired archive: {e:#}")))?;
        if graph.config != config || graph.platform_manifest != platform_digest {
            return Err(failed("acquired archive graph differs from the resolution"));
        }
        let (archive_sha256, archive_bytes) = super::sha256_file(archive)?;
        Ok((
            root,
            platform_digest,
            config,
            layers,
            fetcher.content,
            archive_sha256,
            archive_bytes,
        ))
    })();
    let egress = gate.stop();
    if result.is_err() && archive.exists() {
        let _ = std::fs::remove_file(archive);
    }
    let (
        root_digest,
        platform_manifest_digest,
        config_digest,
        layer_digests,
        content_bytes,
        archive_sha256,
        archive_bytes,
    ) = result?;
    Ok(AcquiredBase {
        reference: reference.to_owned(),
        repository: hub.repository,
        tag: hub.tag,
        platform: platform.to_owned(),
        root_digest,
        platform_manifest_digest,
        config_digest,
        layer_digests,
        content_bytes,
        archive_sha256,
        archive_bytes,
        egress,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_docker_io_name_tag_references_are_acquired() {
        assert_eq!(
            parse_hub_reference("node:24-alpine").unwrap(),
            HubReference {
                repository: "library/node".into(),
                tag: "24-alpine".into()
            }
        );
        assert_eq!(
            parse_hub_reference("docker.io/library/node:24-alpine")
                .unwrap()
                .repository,
            "library/node"
        );
        for bad in [
            "ghcr.io/x/y:1",
            "node",
            "node@sha256:0",
            "Node:1",
            "a/b/c:1",
            "../x:1",
            "node:",
        ] {
            assert_eq!(
                parse_hub_reference(bad).unwrap_err().code,
                "source_oci_base_reference_invalid",
                "{bad}"
            );
        }
    }
}
