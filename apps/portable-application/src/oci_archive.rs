//! Validate a Docker 29 OCI-layout save archive as transport bytes.
//! No tar member is extracted or executed while checking its pinned graph.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};

use anyhow::{Context, Result, ensure};
use ato_objects::PortableOciArchive;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_MEMBER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEMBERS: usize = 32;

pub struct ValidatedOciArchive {
    pub bytes: Vec<u8>,
    pub config_reference: String,
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
    let mut tar = tar::Archive::new(Cursor::new(bytes.as_slice()));
    let mut members = BTreeMap::<String, Vec<u8>>::new();
    for entry in tar.entries().context("read OCI archive")? {
        let mut entry = entry.context("read OCI archive entry")?;
        let header = entry.header();
        ensure!(
            header.entry_type().is_file() || header.entry_type().is_dir(),
            "OCI archive contains a non-file member"
        );
        let path = entry
            .path()
            .context("read OCI archive member path")?
            .into_owned();
        let path = path
            .to_str()
            .context("OCI archive path is not UTF-8")?
            .to_owned();
        if header.entry_type().is_dir() {
            ensure!(
                matches!(path.as_str(), "blobs/" | "blobs/sha256/"),
                "unexpected OCI archive directory"
            );
            continue;
        }
        ensure!(members.len() < MAX_MEMBERS, "too many OCI archive members");
        ensure!(
            entry.size() <= MAX_MEMBER_BYTES,
            "OCI archive member exceeds bound"
        );
        ensure!(
            matches!(path.as_str(), "index.json" | "manifest.json" | "oci-layout")
                || valid_blob_path(&path),
            "unexpected OCI archive member {path}"
        );
        let mut content = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut content)
            .context("read OCI archive member")?;
        ensure!(
            members.insert(path.clone(), content).is_none(),
            "duplicate OCI archive member {path}"
        );
    }
    let layout: Value = serde_json::from_slice(
        members
            .get("oci-layout")
            .context("OCI layout marker missing")?,
    )?;
    ensure!(
        layout["imageLayoutVersion"] == "1.0.0",
        "unsupported OCI layout version"
    );
    let index: Value =
        serde_json::from_slice(members.get("index.json").context("OCI index missing")?)?;
    let index_manifests = index["manifests"]
        .as_array()
        .context("OCI index manifest list missing")?;
    ensure!(
        index_manifests.len() == 1
            && index_manifests[0]["digest"] == format!("sha256:{manifest_digest}"),
        "OCI index does not name pinned manifest"
    );
    let legacy: Value = serde_json::from_slice(
        members
            .get("manifest.json")
            .context("Docker load manifest missing")?,
    )?;
    let legacy = legacy
        .as_array()
        .context("Docker load manifest is not an array")?;
    ensure!(
        legacy.len() == 1,
        "OCI archive must contain exactly one image"
    );
    ensure!(
        legacy[0]["RepoTags"].is_null()
            || legacy[0]["RepoTags"].as_array().is_some_and(Vec::is_empty),
        "OCI archive must not write repository tags"
    );

    let manifest_path = format!("blobs/sha256/{manifest_digest}");
    let manifest_bytes = members
        .get(&manifest_path)
        .context("pinned OCI manifest missing")?;
    ensure!(
        sha256(manifest_bytes) == format!("sha256:{manifest_digest}"),
        "OCI manifest digest failure"
    );
    let manifest: Value = serde_json::from_slice(manifest_bytes).context("decode OCI manifest")?;
    ensure!(
        manifest["schemaVersion"] == 2,
        "unsupported OCI manifest schema"
    );
    let config = &manifest["config"];
    let config_ref = checked_descriptor(config, &members)?;
    let layers = manifest["layers"]
        .as_array()
        .context("OCI manifest has no layers")?;
    ensure!(
        !layers.is_empty() && layers.len() <= 24,
        "OCI layer count is unsupported"
    );
    let mut expected = BTreeSet::from([format!("sha256:{manifest_digest}"), config_ref.clone()]);
    let mut legacy_layers = Vec::new();
    for layer in layers {
        let reference = checked_descriptor(layer, &members)?;
        ensure!(expected.insert(reference.clone()), "duplicate OCI layer");
        legacy_layers.push(format!(
            "blobs/sha256/{}",
            reference.trim_start_matches("sha256:")
        ));
    }
    ensure!(
        legacy[0]["Config"] == format!("blobs/sha256/{}", config_ref.trim_start_matches("sha256:")),
        "Docker load config differs from OCI manifest"
    );
    ensure!(
        legacy[0]["Layers"] == serde_json::to_value(legacy_layers)?,
        "Docker load layers differ from OCI manifest"
    );
    let config_path = format!("blobs/sha256/{}", config_ref.trim_start_matches("sha256:"));
    let config_json: Value =
        serde_json::from_slice(&members[&config_path]).context("decode OCI config")?;
    let platform = format!(
        "{}/{}",
        config_json["os"].as_str().unwrap_or(""),
        config_json["architecture"].as_str().unwrap_or("")
    );
    ensure!(
        platform == archive.platform,
        "OCI archive platform mismatch"
    );
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
        config_reference: config_ref,
    })
}

fn checked_descriptor(descriptor: &Value, members: &BTreeMap<String, Vec<u8>>) -> Result<String> {
    let reference = descriptor["digest"]
        .as_str()
        .context("OCI descriptor lacks digest")?;
    let digest = reference
        .strip_prefix("sha256:")
        .context("OCI descriptor is not SHA-256")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "OCI descriptor digest is invalid"
    );
    let size = descriptor["size"]
        .as_u64()
        .context("OCI descriptor lacks size")?;
    let path = format!("blobs/sha256/{digest}");
    let bytes = members
        .get(&path)
        .with_context(|| format!("OCI blob {reference} missing"))?;
    ensure!(
        bytes.len() as u64 == size && sha256(bytes) == reference,
        "OCI blob {reference} digest/size failure"
    );
    Ok(reference.to_owned())
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

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

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
