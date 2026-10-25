//! Validate a Docker 29 OCI-layout save archive as transport bytes.
//! No tar member is extracted or executed while checking its pinned graph.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};

use anyhow::{Context, Result, ensure};
use ato_objects::PortableOciArchive;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Portable OCI transport bound (raised from 128 MiB to 512 MiB on
/// 2026-09-30 for the WBO source build; user-approved).
pub const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;
const MAX_MEMBER_BYTES: u64 = MAX_ARCHIVE_BYTES as u64;
const MAX_MEMBERS: usize = 32;

pub struct ValidatedOciArchive {
    pub bytes: Vec<u8>,
    pub config_reference: String,
}

/// One tar member as seen by a single streaming pass: its size, its content
/// digest and, for small members, its bytes (index, manifests, configs).
pub(crate) struct Member {
    pub size: u64,
    pub digest: String,
    pub bytes: Option<Vec<u8>>,
}

pub(crate) struct ScanLimits {
    pub max_members: usize,
    pub max_member_bytes: u64,
    pub max_total_bytes: u64,
    /// Members up to this size are kept in memory; larger ones are hashed only.
    pub keep_bytes_up_to: u64,
}

/// Read every member of an OCI-layout save once. Only regular files at the
/// layout's own names or `blobs/sha256/<hex>` are accepted; links, devices,
/// other directories and a repeated member name are refused, so no later
/// entry can replace one that was already checked.
pub(crate) fn scan_members(
    reader: impl Read,
    limits: &ScanLimits,
) -> Result<BTreeMap<String, Member>> {
    let mut tar = tar::Archive::new(reader);
    let mut members = BTreeMap::<String, Member>::new();
    let mut total = 0_u64;
    for entry in tar.entries().context("read OCI archive")? {
        let mut entry = entry.context("read OCI archive entry")?;
        let header = entry.header();
        ensure!(
            header.entry_type().is_file() || header.entry_type().is_dir(),
            "OCI archive contains a non-file member"
        );
        let is_dir = header.entry_type().is_dir();
        let path = entry
            .path()
            .context("read OCI archive member path")?
            .into_owned();
        let path = path
            .to_str()
            .context("OCI archive path is not UTF-8")?
            .to_owned();
        if is_dir {
            ensure!(
                matches!(path.as_str(), "blobs/" | "blobs/sha256/"),
                "unexpected OCI archive directory"
            );
            continue;
        }
        ensure!(
            members.len() < limits.max_members,
            "too many OCI archive members"
        );
        let size = entry.size();
        ensure!(
            size <= limits.max_member_bytes,
            "OCI archive member exceeds bound"
        );
        total = total.saturating_add(size);
        ensure!(
            total <= limits.max_total_bytes,
            "OCI archive exceeds bounded size"
        );
        ensure!(
            matches!(path.as_str(), "index.json" | "manifest.json" | "oci-layout")
                || valid_blob_path(&path),
            "unexpected OCI archive member {path}"
        );
        ensure!(
            !members.contains_key(&path),
            "duplicate OCI archive member {path}"
        );
        let mut hasher = Sha256::new();
        let mut kept = (size <= limits.keep_bytes_up_to).then(|| Vec::with_capacity(size as usize));
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut read = 0_u64;
        loop {
            let n = entry.read(&mut buffer).context("read OCI archive member")?;
            if n == 0 {
                break;
            }
            read += n as u64;
            hasher.update(&buffer[..n]);
            if let Some(kept) = kept.as_mut() {
                kept.extend_from_slice(&buffer[..n]);
            }
        }
        ensure!(read == size, "OCI archive member is truncated");
        members.insert(
            path,
            Member {
                size,
                digest: format!("sha256:{:x}", hasher.finalize()),
                bytes: kept,
            },
        );
    }
    Ok(members)
}

pub(crate) fn member_json(
    members: &BTreeMap<String, Member>,
    path: &str,
    what: &str,
) -> Result<Value> {
    let bytes = members
        .get(path)
        .with_context(|| format!("{what} missing"))?
        .bytes
        .as_deref()
        .with_context(|| format!("{what} exceeds bound"))?;
    serde_json::from_slice(bytes).with_context(|| format!("decode {what}"))
}

/// One image's verified graph: manifest -> config + layers, every blob present
/// with the descriptor's digest and size.
pub(crate) struct ImageGraph {
    pub manifest: String,
    pub config: String,
    pub layers: Vec<String>,
}

pub(crate) fn image_graph(
    manifest_digest: &str,
    members: &BTreeMap<String, Member>,
    platform: &str,
    max_layers: usize,
) -> Result<ImageGraph> {
    let manifest_path = blob_path(manifest_digest)?;
    let entry = members
        .get(&manifest_path)
        .context("pinned OCI manifest missing")?;
    ensure!(
        entry.digest == manifest_digest,
        "OCI manifest digest failure"
    );
    let manifest = member_json(members, &manifest_path, "OCI manifest")?;
    ensure!(
        manifest["schemaVersion"] == 2,
        "unsupported OCI manifest schema"
    );
    let config = checked_descriptor(&manifest["config"], members)?;
    let layers = manifest["layers"]
        .as_array()
        .context("OCI manifest has no layers")?;
    ensure!(
        !layers.is_empty() && layers.len() <= max_layers,
        "OCI layer count is unsupported"
    );
    let mut seen = BTreeSet::from([manifest_digest.to_owned(), config.clone()]);
    let mut layer_refs = Vec::new();
    for layer in layers {
        let reference = checked_descriptor(layer, members)?;
        ensure!(seen.insert(reference.clone()), "duplicate OCI layer");
        layer_refs.push(reference);
    }
    let config_json = member_json(members, &blob_path(&config)?, "OCI config")?;
    let actual = format!(
        "{}/{}",
        config_json["os"].as_str().unwrap_or(""),
        config_json["architecture"].as_str().unwrap_or("")
    );
    ensure!(actual == platform, "OCI archive platform mismatch");
    Ok(ImageGraph {
        manifest: manifest_digest.to_owned(),
        config,
        layers: layer_refs,
    })
}

/// Docker loads `manifest.json`, not the OCI index: it must name exactly the
/// config and layers of the verified graph, or a load could install an image
/// the pinned root never referenced.
pub(crate) fn check_docker_load_manifest(
    members: &BTreeMap<String, Member>,
    graph: &ImageGraph,
) -> Result<Value> {
    let legacy = member_json(members, "manifest.json", "Docker load manifest")?;
    let entries = legacy
        .as_array()
        .context("Docker load manifest is not an array")?;
    ensure!(
        entries.len() == 1,
        "OCI archive must contain exactly one image"
    );
    ensure!(
        entries[0]["Config"] == blob_path(&graph.config)?,
        "Docker load config differs from OCI manifest"
    );
    let layers = graph
        .layers
        .iter()
        .map(|layer| blob_path(layer))
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        entries[0]["Layers"] == serde_json::to_value(layers)?,
        "Docker load layers differ from OCI manifest"
    );
    Ok(entries[0].clone())
}

pub fn verify_oci_archive(archive: &PortableOciArchive) -> Result<ValidatedOciArchive> {
    let (_, manifest_digest) = archive
        .image
        .rsplit_once("@sha256:")
        .context("OCI archive image is not digest-pinned")?;
    ensure!(
        manifest_digest.len() == 64
            && manifest_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "OCI archive image digest is invalid"
    );
    let manifest_digest = format!("sha256:{manifest_digest}");
    ensure!(
        archive.bytes.len() <= MAX_ARCHIVE_BYTES * 4 / 3 + 4,
        "OCI archive exceeds bounded transport size"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&archive.bytes)
        .context("OCI archive is not Base64")?;
    ensure!(
        bytes.len() <= MAX_ARCHIVE_BYTES,
        "OCI archive exceeds bounded transport size"
    );
    let members = scan_members(
        Cursor::new(bytes.as_slice()),
        &ScanLimits {
            max_members: MAX_MEMBERS,
            max_member_bytes: MAX_MEMBER_BYTES,
            max_total_bytes: MAX_ARCHIVE_BYTES as u64,
            keep_bytes_up_to: MAX_MEMBER_BYTES,
        },
    )?;
    let layout = member_json(&members, "oci-layout", "OCI layout marker")?;
    ensure!(
        layout["imageLayoutVersion"] == "1.0.0",
        "unsupported OCI layout version"
    );
    let index = member_json(&members, "index.json", "OCI index")?;
    let index_manifests = index["manifests"]
        .as_array()
        .context("OCI index manifest list missing")?;
    ensure!(
        index_manifests.len() == 1 && index_manifests[0]["digest"] == manifest_digest,
        "OCI index does not name pinned manifest"
    );
    let graph = image_graph(&manifest_digest, &members, &archive.platform, 24)?;
    let legacy = check_docker_load_manifest(&members, &graph)?;
    ensure!(
        legacy["RepoTags"].is_null() || legacy["RepoTags"].as_array().is_some_and(Vec::is_empty),
        "OCI archive must not write repository tags"
    );
    let mut expected = BTreeSet::from([graph.manifest.clone(), graph.config.clone()]);
    expected.extend(graph.layers.iter().cloned());
    for path in members
        .keys()
        .filter(|path| path.starts_with("blobs/sha256/"))
    {
        let reference = format!("sha256:{}", path.trim_start_matches("blobs/sha256/"));
        ensure!(
            expected.contains(&reference),
            "unreferenced OCI blob {reference}"
        );
    }
    Ok(ValidatedOciArchive {
        bytes,
        config_reference: graph.config,
    })
}

/// A frozen base image archive, as produced by `docker save` of one pulled
/// reference. Its `index.json` must name exactly the pinned root; the root is
/// either the platform manifest itself or an image index from which exactly
/// one manifest for `platform` is selected. Every blob's bytes must match its
/// name, and the Docker load manifest must name the selected graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseGraph {
    pub root: String,
    pub platform_manifest: String,
    pub config: String,
    pub layers: Vec<String>,
}

pub const MAX_BASE_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;

pub fn verify_base_archive(
    reader: impl Read,
    pinned_root: &str,
    platform: &str,
) -> Result<BaseGraph> {
    let members = scan_members(
        reader,
        &ScanLimits {
            max_members: 256,
            max_member_bytes: MAX_BASE_ARCHIVE_BYTES,
            max_total_bytes: MAX_BASE_ARCHIVE_BYTES,
            keep_bytes_up_to: 4 * 1024 * 1024,
        },
    )?;
    for (path, member) in &members {
        if let Some(hex) = path.strip_prefix("blobs/sha256/") {
            ensure!(
                member.digest == format!("sha256:{hex}"),
                "base blob {path} does not match its name"
            );
        }
    }
    let index = member_json(&members, "index.json", "OCI index")?;
    let roots = index["manifests"]
        .as_array()
        .context("OCI index manifest list missing")?;
    ensure!(
        roots.len() == 1 && roots[0]["digest"] == pinned_root,
        "OCI index root is not the pinned digest"
    );
    let root = checked_descriptor(&roots[0], &members)?;
    let root_json = member_json(&members, &blob_path(&root)?, "OCI root")?;
    let platform_manifest = if root_json.get("manifests").is_some() {
        select_platform(&root_json, &members, platform)?
    } else {
        root.clone()
    };
    let graph = image_graph(&platform_manifest, &members, platform, 128)?;
    check_docker_load_manifest(&members, &graph)?;
    Ok(BaseGraph {
        root,
        platform_manifest,
        config: graph.config,
        layers: graph.layers,
    })
}

fn select_platform(
    index: &Value,
    members: &BTreeMap<String, Member>,
    platform: &str,
) -> Result<String> {
    checked_descriptor(platform_descriptor(index, platform)?, members)
}

/// The one descriptor of an image index for `platform` (`os/arch`; arm64
/// may carry variant v8). Attestation manifests (`unknown/unknown`) never
/// match; zero or several matches are refused.
pub(crate) fn platform_descriptor<'a>(index: &'a Value, platform: &str) -> Result<&'a Value> {
    let (os, architecture) = platform.split_once('/').context("platform os/arch")?;
    let matches = index["manifests"]
        .as_array()
        .context("OCI index manifest list missing")?
        .iter()
        .filter(|descriptor| {
            let p = &descriptor["platform"];
            let variant = p["variant"].as_str();
            p["os"] == os
                && p["architecture"] == architecture
                && match architecture {
                    "arm64" => variant.is_none_or(|v| v == "v8"),
                    _ => variant.is_none(),
                }
        })
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "OCI index has {} manifests for {platform}; exactly one is required",
        matches.len()
    );
    Ok(matches[0])
}

fn checked_descriptor(descriptor: &Value, members: &BTreeMap<String, Member>) -> Result<String> {
    let reference = descriptor["digest"]
        .as_str()
        .context("OCI descriptor lacks digest")?;
    let path = blob_path(reference)?;
    let size = descriptor["size"]
        .as_u64()
        .context("OCI descriptor lacks size")?;
    let member = members
        .get(&path)
        .with_context(|| format!("OCI blob {reference} missing"))?;
    ensure!(
        member.size == size && member.digest == reference,
        "OCI blob {reference} digest/size failure"
    );
    Ok(reference.to_owned())
}

pub(crate) fn blob_path(reference: &str) -> Result<String> {
    let digest = reference
        .strip_prefix("sha256:")
        .context("OCI descriptor is not SHA-256")?;
    let path = format!("blobs/sha256/{digest}");
    ensure!(valid_blob_path(&path), "OCI descriptor digest is invalid");
    Ok(path)
}

fn valid_blob_path(path: &str) -> bool {
    let Some(digest) = path.strip_prefix("blobs/sha256/") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    fn fixture(corrupt_layer: bool) -> PortableOciArchive {
        let config = br#"{"architecture":"amd64","os":"linux"}"#;
        let layer = b"fixed compressed layer";
        let config_ref = sha256(config);
        let layer_ref = sha256(layer);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "config": {"digest": config_ref, "size": config.len()},
            "layers": [{"digest": layer_ref, "size": layer.len()}]
        }))
        .unwrap();
        let manifest_ref = sha256(&manifest);
        let index = serde_json::to_vec(&serde_json::json!({
            "manifests": [{"digest": manifest_ref}]
        }))
        .unwrap();
        let legacy = serde_json::to_vec(&serde_json::json!([{
            "Config": format!("blobs/sha256/{}", config_ref.trim_start_matches("sha256:")),
            "Layers": [format!("blobs/sha256/{}", layer_ref.trim_start_matches("sha256:"))],
            "RepoTags": null
        }]))
        .unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in [
            (
                "oci-layout".to_owned(),
                br#"{"imageLayoutVersion":"1.0.0"}"#.as_slice(),
            ),
            ("index.json".to_owned(), index.as_slice()),
            ("manifest.json".to_owned(), legacy.as_slice()),
            (
                format!(
                    "blobs/sha256/{}",
                    manifest_ref.trim_start_matches("sha256:")
                ),
                manifest.as_slice(),
            ),
            (
                format!("blobs/sha256/{}", config_ref.trim_start_matches("sha256:")),
                config.as_slice(),
            ),
            (
                format!("blobs/sha256/{}", layer_ref.trim_start_matches("sha256:")),
                if corrupt_layer {
                    b"modified compressed layer".as_slice()
                } else {
                    layer.as_slice()
                },
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, content).unwrap();
        }
        let bytes = builder.into_inner().unwrap();
        PortableOciArchive {
            image: format!("docker.io/example/app@{manifest_ref}"),
            platform: "linux/amd64".to_owned(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    #[test]
    fn verifies_pinned_manifest_config_and_every_layer() {
        let archive = fixture(false);
        let verified = verify_oci_archive(&archive).unwrap();
        assert!(verified.config_reference.starts_with("sha256:"));
    }

    #[test]
    fn altered_layer_fails_before_docker_load() {
        let archive = fixture(true);
        assert!(verify_oci_archive(&archive).is_err());
    }
}
