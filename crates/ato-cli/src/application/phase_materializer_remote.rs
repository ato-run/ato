//! Optional read-only remote CAS mirror for phase build-output layers.
//!
//! This MVP intentionally uses only a file-backed mirror selected by
//! `ATO_PHASE_MATERIALIZATION_REMOTE_ROOT`. It does not submit builds, speak
//! HTTP, or trust remote metadata without re-validating the blob locally.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::blob::{hash_tree, BlobManifest};
use capsule_core::common::store::BlobAddress;
use serde::Deserialize;
use serde_json::Value;

use crate::application::build_materialization::BuildObservation;
use crate::application::phase_materializer::{
    materialization_key_for_observation, materialization_key_path_component,
    validate_build_output_layer_metadata, verify_local_build_output_blob, BuildOutputLayerRecord,
};
use crate::application::projection::project_payload;

const REMOTE_ROOT_ENV: &str = "ATO_PHASE_MATERIALIZATION_REMOTE_ROOT";
const REMOTE_LAYER_SCHEMA_VERSION: &str = "ato-remote-build-output-layer-v1";

#[derive(Debug, Deserialize)]
struct RemoteBuildOutputLayerRecord {
    schema_version: String,
    materialization_key: String,
    blob_hash: String,
    output_contract_digest: String,
    platform_profile: String,
    outputs: Vec<String>,
    #[serde(default)]
    provenance: Option<Value>,
}

impl RemoteBuildOutputLayerRecord {
    fn into_layer(self) -> BuildOutputLayerRecord {
        BuildOutputLayerRecord {
            materialization_key: self.materialization_key,
            blob_hash: self.blob_hash,
            output_contract_digest: self.output_contract_digest,
            platform_profile: self.platform_profile,
            outputs: self.outputs,
        }
    }
}

pub(crate) fn lookup_remote_build_output_layer(
    _workspace_root: &Path,
    observation: &BuildObservation,
) -> Result<Option<BuildOutputLayerRecord>> {
    let Some(remote_root) = remote_root() else {
        return Ok(None);
    };

    let materialization_key = materialization_key_for_observation(observation)
        .context("failed to compute remote build output materialization key")?;
    let remote_layer_root = remote_root
        .join("build-output")
        .join(materialization_key_path_component(&materialization_key));
    let layer_path = remote_layer_root.join("layer.json");
    if !layer_path.exists() {
        return Ok(None);
    }

    let remote_record = read_remote_layer_record(&layer_path).with_context(|| {
        format!(
            "remote materialization layer metadata is invalid at {}",
            layer_path.display()
        )
    })?;
    if remote_record.schema_version != REMOTE_LAYER_SCHEMA_VERSION {
        anyhow::bail!(
            "remote materialization layer schema mismatch: expected {}, got {}",
            REMOTE_LAYER_SCHEMA_VERSION,
            remote_record.schema_version
        );
    }

    let _provenance = remote_record.provenance.as_ref();
    let layer = remote_record.into_layer();
    if layer.materialization_key != materialization_key {
        anyhow::bail!(
            "remote materialization key mismatch: expected {}, got {}",
            materialization_key,
            layer.materialization_key
        );
    }
    validate_build_output_layer_metadata(observation, &layer).context(
        "remote materialization layer metadata does not match current build observation",
    )?;
    verify_remote_blob(&remote_layer_root.join("blob"), &layer.blob_hash)
        .context("remote materialization blob verification failed")?;

    import_remote_blob(&remote_layer_root.join("blob"), &layer.blob_hash)
        .context("failed to import remote materialization blob into local CAS")?;
    verify_local_build_output_blob(&layer.blob_hash)
        .context("imported remote materialization blob failed local CAS verification")?;

    Ok(Some(layer))
}

fn remote_root() -> Option<PathBuf> {
    let value = std::env::var_os(REMOTE_ROOT_ENV)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

fn read_remote_layer_record(path: &Path) -> Result<RemoteBuildOutputLayerRecord> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse remote layer metadata {}", path.display()))
}

fn verify_remote_blob(remote_blob_root: &Path, blob_hash: &str) -> Result<()> {
    let manifest_path = remote_blob_root.join("manifest.json");
    let payload = remote_blob_root.join("payload");
    let manifest = BlobManifest::read_from(&manifest_path).with_context(|| {
        format!(
            "failed to read remote build output blob manifest {}",
            manifest_path.display()
        )
    })?;
    if !manifest.matches_blob_hash(blob_hash) {
        anyhow::bail!(
            "remote materialization blob manifest hash mismatch: expected {}, got {}",
            blob_hash,
            manifest.blob_hash
        );
    }
    let actual = hash_tree(&payload).with_context(|| {
        format!(
            "failed to hash remote build output payload {}",
            payload.display()
        )
    })?;
    if actual.blob_hash != blob_hash {
        anyhow::bail!(
            "remote materialization payload hash mismatch: expected {}, got {}",
            blob_hash,
            actual.blob_hash
        );
    }
    if manifest.file_count != actual.file_count
        || manifest.symlink_count != actual.symlink_count
        || manifest.dir_count != actual.dir_count
        || manifest.total_bytes != actual.total_bytes
    {
        anyhow::bail!(
            "remote materialization blob manifest statistics mismatch for {}",
            blob_hash
        );
    }
    Ok(())
}

fn import_remote_blob(remote_blob_root: &Path, blob_hash: &str) -> Result<()> {
    if verify_local_build_output_blob(blob_hash).is_ok() {
        return Ok(());
    }

    let address = BlobAddress::parse(blob_hash)
        .with_context(|| format!("blob hash {blob_hash} could not be parsed"))?;
    let suffix = format!("remote-{:016x}", rand::random::<u64>());
    let staging = address.staging_dir(&suffix);
    remove_path_if_exists(&staging)?;
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let staging_payload = staging.join("payload");
    project_payload(&remote_blob_root.join("payload"), &staging_payload).with_context(|| {
        format!(
            "failed to stage remote build output payload {} -> {}",
            remote_blob_root.join("payload").display(),
            staging_payload.display()
        )
    })?;
    copy_file(
        &remote_blob_root.join("manifest.json"),
        &staging.join("manifest.json"),
    )?;

    match fs::rename(&staging, address.dir()) {
        Ok(()) => Ok(()),
        Err(_err) if address.dir().is_dir() => {
            let _ = fs::remove_dir_all(&staging);
            verify_local_build_output_blob(blob_hash).with_context(|| {
                format!(
                    "existing local build output blob target is present but failed integrity \
                     check after remote import race: {}",
                    address.dir().display()
                )
            })?;
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to rename remote build output blob staging {} -> {}",
                staging.display(),
                address.dir().display()
            )
        }),
    }
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, target).with_context(|| {
        format!(
            "failed to copy {} -> {}",
            source.display(),
            target.display()
        )
    })?;
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to inspect {}", path.display())),
    }
}
