use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::ato_lock::{compute_closure_digest, compute_lock_id, AtoLock};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::router::ExecutionProfile;
use capsule_core::types::CapsuleManifest;

use crate::application::source_inference::{
    materialize_run_from_canonical_lock, materialize_run_from_compatibility,
    materialize_run_from_source_only, RunMaterialization,
};
use crate::application::workspace::state;
use crate::reporters::CliReporter;

#[derive(Debug)]
pub(crate) struct ProducerAuthoritativeInput {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_raw: String,
    pub(crate) manifest: CapsuleManifest,
    pub(crate) workspace_root: PathBuf,
    pub(crate) lock_path: PathBuf,
    pub(crate) lock: AtoLock,
    pub(crate) advisories: Vec<String>,
    pub(crate) lock_id: Option<String>,
    pub(crate) closure_digest: Option<String>,
    _cleanup: ProducerMaterializationCleanup,
}

#[derive(Debug)]
struct ProducerMaterializationCleanup {
    run_state_dir: PathBuf,
}

impl Drop for ProducerMaterializationCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.run_state_dir);
    }
}

pub(crate) fn resolve_producer_authoritative_input(
    project_root: &Path,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<ProducerAuthoritativeInput> {
    let resolved = resolve_authoritative_input(project_root, ResolveInputOptions::default())?;
    producer_authoritative_input_from_resolved(resolved, reporter, assume_yes)
}

fn producer_authoritative_input_from_resolved(
    resolved: ResolvedInput,
    reporter: Arc<CliReporter>,
    assume_yes: bool,
) -> Result<ProducerAuthoritativeInput> {
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
        let sanitized_lock = state::sanitize_lock_for_distribution(&materialized.lock);
        capsule_core::ato_lock::write_pretty_to_path(&sanitized_lock, &materialized.lock_path)
            .with_context(|| {
                format!(
                    "Failed to rewrite sanitized lock for distribution at {}",
                    materialized.lock_path.display()
                )
            })?;
        let manifest_path = materialized.project_root.join("capsule.toml");
        let manifest_raw = if let Some(raw_manifest) = materialized.raw_manifest.as_ref() {
            toml::to_string(raw_manifest)
                .context("Failed to serialize producer manifest from authoritative source")?
        } else {
            let decision = capsule_core::router::route_lock(
                &materialized.lock_path,
                &sanitized_lock,
                &materialized.project_root,
                ExecutionProfile::Release,
                None,
            )?;
            toml::to_string(&decision.plan.manifest)
                .context("Failed to serialize lock-native producer manifest")?
        };
        let manifest = CapsuleManifest::from_toml(&manifest_raw).map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse producer manifest {}: {}",
                manifest_path.display(),
                err
            )
        })?;
        let run_state_dir = materialized
            .lock_path
            .parent()
            .map(Path::to_path_buf)
            .context("generated lock path is missing parent directory")?;
        let lock_id = compute_lock_id(&sanitized_lock)
            .ok()
            .map(|value| value.as_str().to_string())
            .or_else(|| {
                sanitized_lock
                    .lock_id
                    .as_ref()
                    .map(|value| value.as_str().to_string())
            });
        let closure_digest = sanitized_lock
            .resolution
            .entries
            .get("closure")
            .map(compute_closure_digest)
            .transpose()
            .context("Failed to compute resolution.closure for registry metadata")?
            .flatten();

        Ok(Self {
            manifest_path: manifest_path.clone(),
            manifest_raw,
            manifest,
            workspace_root: materialized.project_root,
            lock_path: materialized.lock_path,
            lock: sanitized_lock,
            advisories,
            lock_id,
            closure_digest,
            _cleanup: ProducerMaterializationCleanup { run_state_dir },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn source_only_producer_materialization_sanitizes_distributed_lock() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{
  "name": "demo",
  "scripts": {
    "start": "node index.js"
  }
}"#,
        )
        .expect("write package json");
        std::fs::write(dir.path().join("index.js"), "console.log('demo');\n")
            .expect("write entrypoint");

        let input = resolve_producer_authoritative_input(
            dir.path(),
            Arc::new(crate::reporters::CliReporter::new(false)),
            true,
        )
        .expect("resolve producer input");

        assert!(input.lock.binding.entries.is_empty());
        assert!(input.lock.binding.unresolved.is_empty());
        assert!(input.lock.attestations.entries.is_empty());
        assert!(input.lock.attestations.unresolved.is_empty());

        let generated_lock = capsule_core::ato_lock::load_unvalidated_from_path(
            &input._cleanup.run_state_dir.join("ato.lock.json"),
        )
        .expect("read generated lock");
        assert!(generated_lock.binding.entries.is_empty());
        assert!(generated_lock.binding.unresolved.is_empty());
        assert!(generated_lock.attestations.entries.is_empty());
        assert!(generated_lock.attestations.unresolved.is_empty());
        assert!(!input.manifest_raw.is_empty());
        assert_eq!(input.workspace_root, dir.path().canonicalize().unwrap());
        assert_eq!(
            input.lock_path.file_name().and_then(|v| v.to_str()),
            Some("ato.lock.json")
        );
    }
}
