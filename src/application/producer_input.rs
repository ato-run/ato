use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::ato_lock::{compute_lock_id, AtoLock};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::lock_runtime::resolve_lock_runtime_model;
use capsule_core::types::CapsuleManifest;
use sha2::{Digest, Sha256};

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
    pub(crate) lock: AtoLock,
    pub(crate) bridge_manifest_sha256: String,
    pub(crate) advisories: Vec<String>,
    pub(crate) lock_id: Option<String>,
    pub(crate) closure_digest: Option<String>,
    _cleanup: ProducerMaterializationCleanup,
}

#[derive(Debug)]
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
    pub(crate) fn validate_bridge_manifest(&self) -> Result<()> {
        let actual_sha256 = sha256_hex(self.manifest_raw.as_bytes());
        if actual_sha256 != self.bridge_manifest_sha256 {
            anyhow::bail!(
                "generated manifest bridge no longer matches the authoritative lock-derived producer input"
            );
        }

        let runtime_model =
            resolve_lock_runtime_model(&self.lock, None).map_err(anyhow::Error::from)?;
        if let Some(expected_name) = runtime_model.metadata.name.as_ref() {
            if self.manifest.name != *expected_name {
                anyhow::bail!(
                    "generated manifest bridge diverged from authoritative lock metadata: manifest name '{}' != lock name '{}'",
                    self.manifest.name,
                    expected_name
                );
            }
        }
        if let Some(expected_version) = runtime_model.metadata.version.as_ref() {
            if self.manifest.version != *expected_version {
                anyhow::bail!(
                    "generated manifest bridge diverged from authoritative lock metadata: manifest version '{}' != lock version '{}'",
                    self.manifest.version,
                    expected_version
                );
            }
        }

        let expected_target = runtime_model
            .metadata
            .default_target
            .clone()
            .unwrap_or_else(|| runtime_model.selected.target_label.clone());
        if self.manifest.default_target != expected_target {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock target selection: manifest default_target '{}' != lock target '{}'",
                self.manifest.default_target,
                expected_target
            );
        }

        let named_targets = self
            .manifest
            .targets
            .as_ref()
            .context("generated manifest bridge is missing [targets]")?;
        let manifest_target = named_targets
            .named
            .get(&self.manifest.default_target)
            .with_context(|| {
                format!(
                    "generated manifest bridge is missing [targets.{}]",
                    self.manifest.default_target
                )
            })?;
        let selected_runtime = &runtime_model.selected.runtime;
        if manifest_target.runtime.trim() != selected_runtime.runtime {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock runtime: target '{}' runtime '{}' != '{}'",
                self.manifest.default_target,
                manifest_target.runtime.trim(),
                selected_runtime.runtime
            );
        }
        if manifest_target.driver.as_deref() != selected_runtime.driver.as_deref() {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock driver: target '{}' driver '{:?}' != '{:?}'",
                self.manifest.default_target,
                manifest_target.driver,
                selected_runtime.driver
            );
        }
        if manifest_target.entrypoint.trim() != selected_runtime.entrypoint {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock entrypoint: target '{}' entrypoint '{}' != '{}'",
                self.manifest.default_target,
                manifest_target.entrypoint.trim(),
                selected_runtime.entrypoint
            );
        }
        if manifest_target.run_command.as_deref() != selected_runtime.run_command.as_deref() {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock run_command: target '{}' run_command '{:?}' != '{:?}'",
                self.manifest.default_target,
                manifest_target.run_command,
                selected_runtime.run_command
            );
        }

        if let Some(services) = self.manifest.services.as_ref() {
            if services.len() != runtime_model.services.len() {
                anyhow::bail!(
                    "generated manifest bridge diverged from authoritative lock orchestration: manifest has {} services but lock resolved {}",
                    services.len(),
                    runtime_model.services.len()
                );
            }

            for service in &runtime_model.services {
                let manifest_service = services.get(&service.name).with_context(|| {
                    format!(
                        "generated manifest bridge is missing [services.{}] required by authoritative lock",
                        service.name
                    )
                })?;
                let manifest_target = manifest_service
                    .target
                    .as_deref()
                    .unwrap_or(self.manifest.default_target.as_str());
                if manifest_target != service.target_label {
                    anyhow::bail!(
                        "generated manifest bridge diverged from authoritative lock orchestration: service '{}' target '{}' != '{}'",
                        service.name,
                        manifest_target,
                        service.target_label
                    );
                }
            }
        }

        Ok(())
    }

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
        let lock_id = compute_lock_id(&sanitized_lock)
            .ok()
            .map(|value| value.as_str().to_string())
            .or_else(|| {
                sanitized_lock
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
            lock: sanitized_lock,
            bridge_manifest_sha256: materialized.bridge_manifest_sha256,
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn producer_bridge_validation_fails_closed_on_target_mismatch() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("generated.capsule.toml");
        let manifest_raw = r#"schema_version = "0.2"
    name = "demo"
    version = "0.1.0"
    type = "app"
    default_target = "other"

    [targets.other]
    runtime = "source"
    driver = "deno"
    entrypoint = "main.ts"
    "#
        .to_string();
        std::fs::write(&manifest_path, &manifest_raw).expect("write manifest");

        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "demo", "version": "0.1.0", "default_target": "default"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "driver": "deno"}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "selected_target": "default"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "default", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"}]),
        );
        lock.resolution
            .entries
            .insert("closure".to_string(), json!({"kind": "metadata_only"}));

        let input = ProducerAuthoritativeInput {
            manifest_path: manifest_path.clone(),
            manifest_raw: manifest_raw.clone(),
            manifest: CapsuleManifest::from_toml(&manifest_raw).expect("parse manifest"),
            lock,
            bridge_manifest_sha256: sha256_hex(manifest_raw.as_bytes()),
            advisories: Vec::new(),
            lock_id: None,
            closure_digest: None,
            _cleanup: ProducerMaterializationCleanup {
                manifest_path,
                run_state_dir: dir.path().join(".tmp"),
            },
        };

        let error = input
            .validate_bridge_manifest()
            .expect_err("target mismatch must fail closed");
        assert!(error.to_string().contains("default_target"));
    }

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
    }
}
