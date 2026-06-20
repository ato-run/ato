//! Optional read-only remote CAS mirror for phase build-output layers.
//!
//! This MVP intentionally uses only a file-backed mirror selected by
//! `ATO_PHASE_MATERIALIZATION_REMOTE_ROOT`. It does not submit builds, speak
//! HTTP, or trust remote metadata without re-validating the blob locally.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule::blob::{BlobManifest, hash_tree};
use capsule::common::store::BlobAddress;
use serde::{Deserialize, Serialize};

use crate::application::build_materialization::{
    BuildObservation, BuildSpecSource, LoadOutcome, MaterializationRecord,
};
use crate::application::phase_materializer::{
    BuildOutputLayerRecord, materialization_key_for_observation,
    materialization_key_path_component, validate_build_output_layer_metadata,
    verify_local_build_output_blob,
};
use crate::application::projection::project_payload;

const REMOTE_ROOT_ENV: &str = "ATO_PHASE_MATERIALIZATION_REMOTE_ROOT";
const REMOTE_LAYER_SCHEMA_VERSION: &str = "ato-remote-build-output-layer-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RemoteBuildOutputLayerRecord {
    schema_version: String,
    materialization_key: String,
    blob_hash: String,
    output_contract_digest: String,
    platform_profile: String,
    outputs: Vec<String>,
    #[serde(default)]
    provenance: Option<RemoteBuildOutputProvenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteBuildOutputProvenance {
    pub(crate) kind: String,
    pub(crate) created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
}

impl RemoteBuildOutputProvenance {
    pub(crate) fn file_mirror_export(created_by: impl Into<String>) -> Self {
        Self {
            kind: "file-mirror-export".to_string(),
            created_by: created_by.into(),
            source: Some("local-cas".to_string()),
        }
    }
}

impl RemoteBuildOutputLayerRecord {
    fn from_layer(layer: &BuildOutputLayerRecord, provenance: RemoteBuildOutputProvenance) -> Self {
        Self {
            schema_version: REMOTE_LAYER_SCHEMA_VERSION.to_string(),
            materialization_key: layer.materialization_key.clone(),
            blob_hash: layer.blob_hash.clone(),
            output_contract_digest: layer.output_contract_digest.clone(),
            platform_profile: layer.platform_profile.clone(),
            outputs: layer.outputs.clone(),
            provenance: Some(provenance),
        }
    }

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

pub(crate) fn export_build_output_layer_to_remote_mirror(
    remote_root: &Path,
    observation: &BuildObservation,
    layer: &BuildOutputLayerRecord,
    provenance: RemoteBuildOutputProvenance,
) -> Result<PathBuf> {
    validate_build_output_layer_metadata(observation, layer)
        .context("local build output layer metadata is not exportable")?;
    let address = verify_local_build_output_blob(&layer.blob_hash)
        .context("local build output layer blob failed export verification")?;
    let expected_remote_record = RemoteBuildOutputLayerRecord::from_layer(layer, provenance);
    let final_dir = remote_layer_dir(remote_root, &layer.materialization_key);

    if final_dir.exists() {
        verify_existing_export(&final_dir, observation, layer)?;
        return Ok(final_dir);
    }

    let staging_dir = export_staging_dir(remote_root, &layer.materialization_key);
    remove_path_if_exists(&staging_dir)?;
    let result = stage_remote_export(&staging_dir, &address, layer, &expected_remote_record);
    if let Err(error) = result {
        let _ = remove_path_if_exists(&staging_dir);
        return Err(error);
    }

    if let Some(parent) = final_dir.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match fs::rename(&staging_dir, &final_dir) {
        Ok(()) => Ok(final_dir),
        Err(_err) if final_dir.exists() => {
            let _ = remove_path_if_exists(&staging_dir);
            verify_existing_export(&final_dir, observation, layer)?;
            Ok(final_dir)
        }
        Err(err) => {
            let _ = remove_path_if_exists(&staging_dir);
            Err(err).with_context(|| {
                format!(
                    "failed to publish remote build output export {} -> {}",
                    staging_dir.display(),
                    final_dir.display()
                )
            })
        }
    }
}

/// Narrow library hook used by E2E coverage until an explicit producer surface
/// decides how to obtain observations and layer records.
pub fn export_recorded_build_output_layer_to_remote_mirror(
    remote_root: &Path,
    workspace_root: &Path,
) -> Result<PathBuf> {
    let (observation, layer) = recorded_build_output_layer(workspace_root)?;
    export_build_output_layer_to_remote_mirror(
        remote_root,
        &observation,
        &layer,
        RemoteBuildOutputProvenance::file_mirror_export("ato-cli-test"),
    )
}

fn remote_root() -> Option<PathBuf> {
    let value = std::env::var_os(REMOTE_ROOT_ENV)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

fn recorded_build_output_layer(
    workspace_root: &Path,
) -> Result<(BuildObservation, BuildOutputLayerRecord)> {
    let file = match crate::application::build_materialization::load_state(workspace_root) {
        LoadOutcome::Loaded(file) => file,
        LoadOutcome::Missing => {
            anyhow::bail!(
                "workspace {} has no build materialization state to export",
                workspace_root.display()
            )
        }
        LoadOutcome::Invalid(_) => {
            anyhow::bail!(
                "workspace {} has invalid build materialization state to export",
                workspace_root.display()
            )
        }
    };
    let record = file
        .artifacts
        .iter()
        .find(|record| record.name == "build" && record.output_layer.is_some())
        .context("build materialization state did not contain an output layer to export")?;
    let layer = record
        .output_layer
        .clone()
        .context("build materialization state did not contain an output layer to export")?;
    Ok((observation_from_record(record), layer))
}

fn observation_from_record(record: &MaterializationRecord) -> BuildObservation {
    BuildObservation {
        source: BuildSpecSource::Declared,
        command: record.command.clone(),
        input_digest: record.input_digest.clone(),
        outputs: record.outputs.clone(),
        target: record.target.clone(),
        working_dir_relative: record.working_dir.clone(),
    }
}

fn read_remote_layer_record(path: &Path) -> Result<RemoteBuildOutputLayerRecord> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse remote layer metadata {}", path.display()))
}

fn write_remote_layer_record(path: &Path, record: &RemoteBuildOutputLayerRecord) -> Result<()> {
    let mut bytes =
        serde_json::to_vec_pretty(record).context("failed to serialize remote layer metadata")?;
    bytes.push(b'\n');
    fs::write(path, bytes)
        .with_context(|| format!("failed to write remote layer metadata {}", path.display()))
}

fn stage_remote_export(
    staging_dir: &Path,
    address: &BlobAddress,
    layer: &BuildOutputLayerRecord,
    remote_record: &RemoteBuildOutputLayerRecord,
) -> Result<()> {
    let blob_dir = staging_dir.join("blob");
    fs::create_dir_all(&blob_dir)
        .with_context(|| format!("failed to create {}", blob_dir.display()))?;
    project_payload(&address.payload_dir(), &blob_dir.join("payload")).with_context(|| {
        format!(
            "failed to stage local build output payload {} -> {}",
            address.payload_dir().display(),
            blob_dir.join("payload").display()
        )
    })?;
    copy_file(&address.manifest_path(), &blob_dir.join("manifest.json"))?;
    verify_remote_blob(&blob_dir, &layer.blob_hash)
        .context("staged remote build output blob verification failed")?;
    write_remote_layer_record(&staging_dir.join("layer.json"), remote_record)?;
    Ok(())
}

fn verify_existing_export(
    final_dir: &Path,
    observation: &BuildObservation,
    expected_layer: &BuildOutputLayerRecord,
) -> Result<()> {
    let layer_path = final_dir.join("layer.json");
    let remote_record = read_remote_layer_record(&layer_path).with_context(|| {
        format!(
            "existing remote build output export is invalid at {}",
            final_dir.display()
        )
    })?;
    if remote_record.schema_version != REMOTE_LAYER_SCHEMA_VERSION {
        anyhow::bail!(
            "existing remote build output export at {} has unsupported schema {}",
            final_dir.display(),
            remote_record.schema_version
        );
    }
    let existing_layer = remote_record.into_layer();
    if existing_layer != *expected_layer {
        anyhow::bail!(
            "existing remote build output export at {} references a different layer",
            final_dir.display()
        );
    }
    validate_build_output_layer_metadata(observation, &existing_layer).with_context(|| {
        format!(
            "existing remote build output export at {} has incompatible metadata",
            final_dir.display()
        )
    })?;
    verify_remote_blob(&final_dir.join("blob"), &existing_layer.blob_hash).with_context(|| {
        format!(
            "existing remote build output export at {} failed blob verification",
            final_dir.display()
        )
    })
}

fn remote_layer_dir(remote_root: &Path, materialization_key: &str) -> PathBuf {
    remote_root
        .join("build-output")
        .join(materialization_key_path_component(materialization_key))
}

fn export_staging_dir(remote_root: &Path, materialization_key: &str) -> PathBuf {
    remote_root.join("build-output").join(format!(
        ".staging-{}-{:016x}",
        materialization_key_path_component(materialization_key),
        rand::random::<u64>()
    ))
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
