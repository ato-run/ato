use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::ato_lock::compute_lock_id;
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::types::CapsuleManifest;

use crate::application::source_inference::{
    materialize_run_from_canonical_lock, materialize_run_from_compatibility,
    materialize_run_from_source_only, RunMaterialization,
};
use crate::reporters::CliReporter;

pub(crate) struct ProducerAuthoritativeInput {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_raw: String,
    pub(crate) manifest: CapsuleManifest,
    pub(crate) advisories: Vec<String>,
    pub(crate) lock_id: Option<String>,
    pub(crate) closure_digest: Option<String>,
    _cleanup: ProducerMaterializationCleanup,
}

struct ProducerMaterializationCleanup {
    manifest_path: PathBuf,
    run_state_dir: PathBuf,
}

impl Drop for ProducerMaterializationCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.manifest_path);
        let _ = fs::remove_dir_all(&self.run_state_dir);
    }
}

pub(crate) fn resolve_producer_authoritative_input(
    project_root: &Path,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<ProducerAuthoritativeInput> {
    let resolved = resolve_authoritative_input(project_root, ResolveInputOptions::default())?;
    match resolved {
        ResolvedInput::CanonicalLock { canonical, .. } => {
            let materialized =
                materialize_run_from_canonical_lock(&canonical, None, reporter, assume_yes)?;
            ProducerAuthoritativeInput::from_materialized(materialized, Vec::new())
        }
        ResolvedInput::CompatibilityProject {
            project,
            advisories,
            ..
        } => {
            let materialized =
                materialize_run_from_compatibility(&project, None, reporter, assume_yes)?;
            ProducerAuthoritativeInput::from_materialized(
                materialized,
                advisories.into_iter().map(|entry| entry.message).collect(),
            )
        }
        ResolvedInput::SourceOnly { source, .. } => {
            let materialized =
                materialize_run_from_source_only(&source, None, reporter, assume_yes)?;
            ProducerAuthoritativeInput::from_materialized(materialized, Vec::new())
        }
    }
}

impl ProducerAuthoritativeInput {
    fn from_materialized(
        materialized: RunMaterialization,
        advisories: Vec<String>,
    ) -> Result<Self> {
        let manifest_raw = fs::read_to_string(&materialized.manifest_path)
            .with_context(|| format!("Failed to read {}", materialized.manifest_path.display()))?;
        let manifest = CapsuleManifest::from_toml(&manifest_raw).map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse generated compatibility manifest {}: {}",
                materialized.manifest_path.display(),
                err
            )
        })?;
        let run_state_dir = materialized
            .lock_path
            .parent()
            .map(Path::to_path_buf)
            .context("generated lock path is missing parent directory")?;
        let lock_id = compute_lock_id(&materialized.lock)
            .ok()
            .map(|value| value.as_str().to_string())
            .or_else(|| {
                materialized
                    .lock
                    .lock_id
                    .as_ref()
                    .map(|value| value.as_str().to_string())
            });
        let closure_digest = materialized
            .lock
            .resolution
            .entries
            .get("closure")
            .map(serde_json::to_vec)
            .transpose()
            .context("Failed to serialize resolution.closure for registry metadata")?
            .map(|bytes| crate::artifact_hash::compute_blake3_label(&bytes));

        Ok(Self {
            manifest_path: materialized.manifest_path.clone(),
            manifest_raw,
            manifest,
            advisories,
            lock_id,
            closure_digest,
            _cleanup: ProducerMaterializationCleanup {
                manifest_path: materialized.manifest_path,
                run_state_dir,
            },
        })
    }
}
