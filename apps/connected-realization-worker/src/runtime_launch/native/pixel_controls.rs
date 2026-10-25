//! Physical metadata for a trusted display tool, after existing OCI validation.
use anyhow::{Context, Result, ensure};
use ato_portable_application::oci_archive::ValidatedOciArchive;
use std::io::{Cursor, Read};

pub(super) const CONTROLS_ABI_LABEL: &str = "org.ato.native-display.controls-abi";
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// This is only a metadata reader. The caller must first obtain `verified`
/// from verify_oci_archive; a marker is never an archive/digest authority.
pub(super) fn verified_controls_abi(verified: &ValidatedOciArchive) -> Result<bool> {
    let digest = verified
        .config_reference
        .strip_prefix("sha256:")
        .context("native_surface_controls_unsupported: missing verified config reference")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "native_surface_controls_unsupported: invalid verified config reference"
    );
    let expected = format!("blobs/sha256/{digest}");
    let mut archive = tar::Archive::new(Cursor::new(&verified.bytes));
    for (count, entry) in archive.entries()?.enumerate() {
        ensure!(
            count < 34,
            "native_surface_controls_unsupported: oversized verified archive metadata"
        );
        let entry = entry?;
        if entry.path()?.to_str() != Some(expected.as_str()) {
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file() && entry.size() <= MAX_CONFIG_BYTES,
            "native_surface_controls_unsupported: helper config exceeds bound"
        );
        let mut bytes = Vec::new();
        entry.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_CONFIG_BYTES,
            "native_surface_controls_unsupported: helper config exceeds bound"
        );
        let config: serde_json::Value = serde_json::from_slice(&bytes)
            .context("native_surface_controls_unsupported: invalid verified helper config")?;
        return Ok(config
            .get("config")
            .and_then(|v| v.get("Labels"))
            .and_then(|v| v.get(CONTROLS_ABI_LABEL))
            .and_then(|v| v.as_str())
            == Some("1"));
    }
    anyhow::bail!("native_surface_controls_unsupported: verified helper config is absent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    fn verified_config(config: &[u8]) -> ValidatedOciArchive {
        // Unit fixture for the reader only; archive authenticity is tested at
        // PixelHelper::from_archive using the existing validator authority.
        let digest = format!("{:x}", Sha256::digest(config));
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(config.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("blobs/sha256/{digest}"), config)
            .unwrap();
        ValidatedOciArchive {
            bytes: tar.into_inner().unwrap(),
            config_reference: format!("sha256:{digest}"),
        }
    }
    #[test]
    fn marker_is_exact_and_legacy_is_not_upgraded() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(true),
            serde_json::json!("0"),
            serde_json::json!("2"),
        ] {
            let config = serde_json::json!({"config":{"Labels":{CONTROLS_ABI_LABEL:value}}});
            assert!(
                !verified_controls_abi(&verified_config(&serde_json::to_vec(&config).unwrap()))
                    .unwrap()
            );
        }
        assert!(!verified_controls_abi(&verified_config(b"{}")).unwrap());
        let config = serde_json::json!({"config":{"Labels":{CONTROLS_ABI_LABEL:"1"}}});
        assert!(
            verified_controls_abi(&verified_config(&serde_json::to_vec(&config).unwrap())).unwrap()
        );
    }
    #[test]
    fn reader_is_bounded_and_uses_the_exact_verified_config() {
        assert!(
            verified_controls_abi(&verified_config(&vec![b' '; MAX_CONFIG_BYTES as usize + 1]))
                .is_err()
        );
        let mut fixture = verified_config(b"{}");
        fixture.config_reference = format!("sha256:{}", "a".repeat(64));
        assert!(verified_controls_abi(&fixture).is_err());
    }
    fn archive_fixture(marker: bool, corrupt_layer: bool) -> ato_objects::PortableOciArchive {
        use base64::Engine;
        let config = serde_json::to_vec(&serde_json::json!({"architecture":"amd64","os":"linux","config":{"Labels":{CONTROLS_ABI_LABEL:if marker {"1"} else {"0"}}}})).unwrap();
        let layer = b"fixed fixture layer";
        let digest = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
        let config_ref = digest(&config);
        let layer_ref = digest(layer);
        let manifest=serde_json::to_vec(&serde_json::json!({"schemaVersion":2,"config":{"digest":config_ref,"size":config.len()},"layers":[{"digest":layer_ref,"size":layer.len()}]})).unwrap();
        let manifest_ref = digest(&manifest);
        let index = serde_json::to_vec(&serde_json::json!({"manifests":[{"digest":manifest_ref}]}))
            .unwrap();
        let blob =
            |reference: &str| format!("blobs/sha256/{}", reference.trim_start_matches("sha256:"));
        let legacy=serde_json::to_vec(&serde_json::json!([{"Config":blob(&config_ref),"Layers":[blob(&layer_ref)],"RepoTags":null}])).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in [
            (
                "oci-layout".into(),
                br#"{"imageLayoutVersion":"1.0.0"}"#.as_slice(),
            ),
            ("index.json".into(), index.as_slice()),
            ("manifest.json".into(), legacy.as_slice()),
            (blob(&manifest_ref), manifest.as_slice()),
            (blob(&config_ref), config.as_slice()),
            (
                blob(&layer_ref),
                if corrupt_layer {
                    b"changed fixture data".as_slice()
                } else {
                    layer.as_slice()
                },
            ),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes).unwrap();
        }
        ato_objects::PortableOciArchive {
            image: format!("example.invalid/trusted-helper@{manifest_ref}"),
            platform: "linux/amd64".into(),
            bytes: base64::engine::general_purpose::STANDARD.encode(builder.into_inner().unwrap()),
        }
    }
    fn limits() -> ato_adapter_oci::OciResourceLimits {
        ato_adapter_oci::OciResourceLimits {
            memory_bytes: 128 * 1024 * 1024,
            cpu_limit_millis: 500,
            pids_limit: 32,
        }
    }
    #[test]
    fn controls_require_existing_digest_authority_and_exact_marker() {
        use super::super::pixel::PixelHelper;
        use netd::pixel_gateway::PixelGatewayControls;
        let controls = PixelGatewayControls {
            text_paste: true,
            framebuffer_resize: false,
        };
        assert!(
            PixelHelper::from_archive_with_controls(
                archive_fixture(true, true),
                limits(),
                controls
            )
            .is_err()
        );
        let error = PixelHelper::from_archive_with_controls(
            archive_fixture(false, false),
            limits(),
            controls,
        )
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .starts_with("native_surface_controls_unsupported:")
        );
        let legacy = PixelHelper::from_archive(archive_fixture(false, false), limits()).unwrap();
        assert!(!legacy.controls().text_paste && !legacy.controls().framebuffer_resize);
        let enabled = PixelHelper::from_archive_with_controls(
            archive_fixture(true, false),
            limits(),
            controls,
        )
        .unwrap();
        assert!(enabled.controls().text_paste && !enabled.controls().framebuffer_resize);
        let mut wrong = archive_fixture(true, false);
        wrong.image = format!("example.invalid/helper@sha256:{}", "00".repeat(32));
        assert!(PixelHelper::from_archive_with_controls(wrong, limits(), controls).is_err());
    }
}
