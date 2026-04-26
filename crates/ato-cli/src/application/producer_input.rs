use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use capsule_core::ato_lock::{compute_closure_digest, compute_lock_id};
use capsule_core::input_resolver::{
    resolve_authoritative_input, ResolveInputOptions, ResolvedInput,
};
use capsule_core::lock_runtime::resolve_lock_runtime_model;
use capsule_core::router::{CompatManifestBridge, CompatProjectInput, ExecutionDescriptor};
use serde_json::Value;

use crate::application::ports::publish::{PublishArtifactIdentityClass, PublishArtifactMetadata};
use crate::application::source_inference::{
    materialize_run_from_canonical_lock, materialize_run_from_compatibility,
    materialize_run_from_source_only, RunMaterialization,
};
use crate::application::workspace::state;
use crate::reporters::CliReporter;

#[derive(Debug)]
pub(crate) struct ProducerAuthoritativeInput {
    pub(crate) descriptor: ExecutionDescriptor,
    pub(crate) advisories: Vec<String>,
    pub(crate) lock_id: Option<String>,
    pub(crate) closure_digest: Option<String>,
    legacy_producer_bridge: Option<LegacyProducerBridge>,
    _cleanup: ProducerMaterializationCleanup,
}

#[derive(Debug)]
struct ProducerMaterializationCleanup {
    run_state_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyProducerBridgeOrigin {
    CompatibilityInput,
    LockDerived,
}

#[derive(Debug, Clone)]
struct LegacyProducerBridge {
    bridge: CompatManifestBridge,
    origin: LegacyProducerBridgeOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopSourcePublishContract {
    pub(crate) framework: String,
    pub(crate) target: String,
    pub(crate) mode: String,
    pub(crate) closure_status: String,
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
    pub(crate) fn semantic_package_name(&self) -> Result<String> {
        self.descriptor
            .runtime_model
            .metadata
            .name
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("authoritative lock metadata is missing package name")
    }

    pub(crate) fn semantic_package_version(&self) -> String {
        self.descriptor
            .runtime_model
            .metadata
            .version
            .clone()
            .unwrap_or_default()
    }

    pub(crate) fn legacy_producer_manifest_value(&self) -> Option<toml::Value> {
        self.legacy_producer_bridge
            .as_ref()
            .and_then(|bridge| bridge.bridge.toml_value().ok())
    }

    pub(crate) fn packaging_compat_project_input(&self) -> Result<Option<CompatProjectInput>> {
        Ok(self
            .legacy_producer_bridge
            .as_ref()
            .map(|bridge| {
                CompatProjectInput::from_bridge(
                    self.descriptor.workspace_root.clone(),
                    bridge.bridge.clone(),
                )
            })
            .transpose()?)
    }

    #[allow(dead_code)]
    pub(crate) fn legacy_producer_manifest_text(&self) -> Option<&str> {
        self.legacy_producer_bridge()
            .map(CompatManifestBridge::manifest_text)
    }

    pub(crate) fn validate_legacy_producer_bridge(&self) -> Result<()> {
        let Some(bridge) = self.legacy_producer_bridge() else {
            return Ok(());
        };
        let actual_sha256 = sha256_hex(bridge.manifest_text().as_bytes());
        if actual_sha256 != bridge.manifest_sha256() {
            anyhow::bail!(
                "generated manifest bridge no longer matches the authoritative lock-derived producer input"
            );
        }

        let runtime_model =
            resolve_lock_runtime_model(&self.descriptor.lock, None).map_err(anyhow::Error::from)?;
        if let Some(expected_name) = runtime_model.metadata.name.as_ref() {
            if bridge.package_name() != expected_name {
                anyhow::bail!(
                    "generated manifest bridge diverged from authoritative lock metadata: manifest name '{}' != lock name '{}'",
                    bridge.package_name(),
                    expected_name
                );
            }
        }
        if let Some(expected_version) = runtime_model.metadata.version.as_ref() {
            if bridge.package_version() != expected_version {
                anyhow::bail!(
                    "generated manifest bridge diverged from authoritative lock metadata: manifest version '{}' != lock version '{}'",
                    bridge.package_version(),
                    expected_version
                );
            }
        }

        let expected_target = runtime_model
            .metadata
            .default_target
            .clone()
            .unwrap_or_else(|| runtime_model.selected.target_label.clone());
        if bridge.manifest_model().default_target != expected_target {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock target selection: manifest default_target '{}' != lock target '{}'",
                bridge.manifest_model().default_target,
                expected_target
            );
        }

        let named_targets = bridge
            .manifest_model()
            .targets
            .as_ref()
            .context("generated manifest bridge is missing [targets]")?;
        let manifest_target = named_targets
            .named
            .get(&bridge.manifest_model().default_target)
            .with_context(|| {
                format!(
                    "generated manifest bridge is missing [targets.{}]",
                    bridge.manifest_model().default_target
                )
            })?;
        let selected_runtime = &runtime_model.selected.runtime;
        if manifest_target.runtime.trim() != selected_runtime.runtime {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock runtime: target '{}' runtime '{}' != '{}'",
                bridge.manifest_model().default_target,
                manifest_target.runtime.trim(),
                selected_runtime.runtime
            );
        }
        if manifest_target.driver.as_deref() != selected_runtime.driver.as_deref() {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock driver: target '{}' driver '{:?}' != '{:?}'",
                bridge.manifest_model().default_target,
                manifest_target.driver,
                selected_runtime.driver
            );
        }
        // schema_version 0.3 manifests reject legacy `entrypoint`/`cmd` fields:
        // execution metadata lives in `run_command`, and lock-synthesized
        // bridges may legitimately omit both when the lock only carries the
        // v0.2-style entrypoint. Skip the per-field strict comparison for v0.3
        // bridges entirely — the lock runtime model is the authority.
        let manifest_ep = manifest_target.entrypoint.trim();
        let lock_ep = selected_runtime.entrypoint.trim();
        let skip_v03_entrypoint_check = bridge.is_schema_v03();
        if !skip_v03_entrypoint_check && manifest_ep != lock_ep {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock entrypoint: target '{}' entrypoint '{}' != '{}'",
                bridge.manifest_model().default_target,
                manifest_ep,
                lock_ep
            );
        }
        if !skip_v03_entrypoint_check
            && manifest_target.run_command.as_deref() != selected_runtime.run_command.as_deref()
        {
            anyhow::bail!(
                "generated manifest bridge diverged from authoritative lock run_command: target '{}' run_command '{:?}' != '{:?}'",
                bridge.manifest_model().default_target,
                manifest_target.run_command,
                selected_runtime.run_command
            );
        }

        if let Some(services) = bridge.manifest_model().services.as_ref() {
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
                    .unwrap_or(bridge.manifest_model().default_target.as_str());
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

    pub(crate) fn compatibility_input_repository(&self) -> Option<String> {
        self.compatibility_input_bridge()
            .and_then(CompatManifestBridge::repository)
    }

    pub(crate) fn compatibility_publish_registry(&self) -> Option<String> {
        self.compatibility_input_bridge()
            .and_then(CompatManifestBridge::publish_registry)
    }

    pub(crate) fn compatibility_store_playground_enabled(&self) -> bool {
        self.compatibility_input_bridge()
            .map(CompatManifestBridge::store_playground_enabled)
            .unwrap_or(false)
    }

    pub(crate) fn publish_metadata(&self) -> Option<PublishArtifactMetadata> {
        publish_metadata_from_lock(&self.descriptor.lock)
    }

    pub(crate) fn publish_metadata_for_source_artifact(
        &self,
        finalized_locally: bool,
    ) -> Option<PublishArtifactMetadata> {
        let mut metadata = self.publish_metadata()?;
        if finalized_locally {
            metadata.identity_class = PublishArtifactIdentityClass::LocallyFinalizedSignedBundle;
            metadata.provenance_limited = false;
        }
        Some(metadata)
    }

    pub(crate) fn serialized_lock_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.descriptor.lock)
            .context("failed to serialize authoritative lock for distribution artifact")
    }

    pub(crate) fn desktop_source_publish_contract(&self) -> Option<DesktopSourcePublishContract> {
        let delivery = self
            .descriptor
            .lock
            .contract
            .entries
            .get("delivery")?
            .as_object()?;
        let artifact = delivery.get("artifact")?.as_object()?;
        if artifact.get("kind").and_then(Value::as_str)? != "desktop-native" {
            return None;
        }

        let mode = delivery
            .get("mode")
            .and_then(Value::as_str)?
            .trim()
            .to_string();
        if !matches!(mode.as_str(), "source-draft" | "source-derivation") {
            return None;
        }

        let framework = artifact
            .get("framework")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let target = artifact
            .get("target")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let closure_status = delivery
            .get("build")
            .and_then(Value::as_object)
            .and_then(|build| build.get("closure_status"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("incomplete")
            .to_string();

        Some(DesktopSourcePublishContract {
            framework,
            target,
            mode,
            closure_status,
        })
    }

    pub(crate) fn ensure_desktop_source_publish_ready(&self) -> Result<()> {
        let Some(contract) = self.desktop_source_publish_contract() else {
            return Ok(());
        };

        if !desktop_source_publish_framework_supported(&contract.framework) {
            anyhow::bail!(
                "desktop source publish currently supports only Tauri, Electron, Wails, or GPUI/Wry (got '{}')",
                contract.framework
            );
        }
        crate::build::native_delivery::ensure_current_host_delivery_target(
            &contract.target,
            "desktop source publish",
        )
    }

    pub(crate) fn ensure_finalize_local_publish_ready(
        &self,
    ) -> Result<DesktopSourcePublishContract> {
        let contract = self.desktop_source_publish_contract().context(
            "--finalize-local is only available for Tauri/Electron/Wails/GPUI-Wry source publish",
        )?;
        self.ensure_desktop_source_publish_ready()?;
        if !crate::build::native_delivery::host_supports_finalize() {
            anyhow::bail!(
                "--finalize-local currently supports only macOS and Windows desktop publish targets"
            );
        }
        Ok(contract)
    }

    fn legacy_producer_bridge(&self) -> Option<&CompatManifestBridge> {
        self.legacy_producer_bridge
            .as_ref()
            .map(|bridge| &bridge.bridge)
    }

    fn compatibility_input_bridge(&self) -> Option<&CompatManifestBridge> {
        self.legacy_producer_bridge.as_ref().and_then(|bridge| {
            (bridge.origin == LegacyProducerBridgeOrigin::CompatibilityInput)
                .then_some(&bridge.bridge)
        })
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
        let lock_decision = capsule_core::router::route_lock(
            &materialized.lock_path,
            &sanitized_lock,
            &materialized.project_root,
            capsule_core::router::ExecutionProfile::Release,
            None,
        )?;
        let legacy_producer_bridge = Some(
            if let Some(raw_manifest) = materialized.raw_manifest.as_ref() {
                LegacyProducerBridge {
                    bridge: CompatManifestBridge::from_manifest_value(raw_manifest).with_context(
                        || {
                            format!(
                                "Failed to build producer manifest bridge from compatibility manifest {}",
                                materialized.project_root.join("capsule.toml").display()
                            )
                        },
                    )?,
                    origin: LegacyProducerBridgeOrigin::CompatibilityInput,
                }
            } else {
                LegacyProducerBridge {
                    bridge: CompatManifestBridge::from_lock(
                        &sanitized_lock,
                        &lock_decision.plan.runtime_model,
                    )
                    .with_context(|| {
                        format!(
                            "Failed to build lock-derived producer manifest bridge {}",
                            materialized.project_root.join("capsule.toml").display()
                        )
                    })?,
                    origin: LegacyProducerBridgeOrigin::LockDerived,
                }
            },
        );
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
            .map(compute_closure_digest)
            .transpose()
            .context("Failed to compute canonical resolution.closure digest for registry metadata")?
            .flatten();

        Ok(Self {
            descriptor: lock_decision.plan,
            advisories,
            lock_id,
            closure_digest,
            legacy_producer_bridge,
            _cleanup: ProducerMaterializationCleanup { run_state_dir },
        })
    }
}

fn desktop_source_publish_framework_supported(framework: &str) -> bool {
    matches!(
        framework.trim(),
        "tauri" | "electron" | "wails" | "gpui-wry"
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(crate) fn publish_metadata_from_lock(
    lock: &capsule_core::ato_lock::AtoLock,
) -> Option<PublishArtifactMetadata> {
    let delivery = lock.contract.entries.get("delivery")?.as_object()?;
    let mode = delivery
        .get("mode")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let artifact = delivery.get("artifact")?.as_object()?;
    if artifact.get("kind").and_then(Value::as_str)? != "desktop-native" {
        return None;
    }

    let identity_class = match mode.as_deref() {
        Some("source-draft") | Some("source-derivation") => {
            PublishArtifactIdentityClass::SourceDerivedUnsignedBundle
        }
        Some("artifact-import") => PublishArtifactIdentityClass::ImportedThirdPartyArtifact,
        _ => return None,
    };

    Some(PublishArtifactMetadata {
        identity_class,
        delivery_mode: mode,
        provenance_limited: artifact
            .get("provenance_limited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::ato_lock::AtoLock;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn desktop_source_publish_framework_support_includes_gpui_wry() {
        assert!(desktop_source_publish_framework_supported("gpui-wry"));
        assert!(desktop_source_publish_framework_supported("tauri"));
        assert!(!desktop_source_publish_framework_supported("custom-native"));
    }

    #[test]
    fn gpui_wry_native_command_project_is_publish_ready() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("capsule.toml"),
            r#"schema_version = "0.3"
name = "desktop-demo"
version = "0.1.0"
type = "app"

runtime = "source/native"
build = "sh build-app.sh"
working_dir = "."
run = "sh run-app.sh"
[artifact]
framework = "gpui-wry"
stage = "unsigned"
target = "darwin/arm64"
input = "dist/Desktop Demo.app"

[finalize]
tool = "codesign"
args = ["--deep", "--force", "--sign", "-", "dist/Desktop Demo.app"]
"#,
        )
        .expect("capsule.toml");
        std::fs::write(
            dir.path().join("build-app.sh"),
            "#!/bin/sh\nset -eu\nmkdir -p 'dist/Desktop Demo.app/Contents/MacOS'\nprintf '#!/bin/sh\necho native\n' > 'dist/Desktop Demo.app/Contents/MacOS/Desktop Demo'\nchmod 755 'dist/Desktop Demo.app/Contents/MacOS/Desktop Demo'\n",
        )
        .expect("build-app.sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(dir.path().join("build-app.sh"))
                .expect("metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(dir.path().join("build-app.sh"), permissions).expect("chmod");
        }

        let input = resolve_producer_authoritative_input(
            dir.path(),
            Arc::new(crate::reporters::CliReporter::new(false)),
            false,
        )
        .expect("resolve producer input");

        let contract = input
            .desktop_source_publish_contract()
            .expect("desktop source publish contract");
        assert_eq!(contract.framework, "gpui-wry");
        assert!(matches!(
            contract.mode.as_str(),
            "source-draft" | "source-derivation"
        ));
        input
            .ensure_desktop_source_publish_ready()
            .expect("gpui-wry native project should be publish ready");
    }

    #[test]
    fn producer_bridge_validation_fails_closed_on_target_mismatch() {
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("generated.capsule.toml");
        let manifest_raw = r#"schema_version = "0.3"
    name = "demo"
    version = "0.1.0"
    type = "app"

runtime = "source/deno"
run = "main.ts""#
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
        let compat_manifest = CompatManifestBridge::from_manifest_value(
            &toml::from_str(&manifest_raw).expect("raw manifest"),
        )
        .expect("compat bridge");
        let descriptor = capsule_core::router::route_lock(
            &manifest_path,
            &lock,
            dir.path(),
            capsule_core::router::ExecutionProfile::Release,
            None,
        )
        .expect("route lock")
        .plan;

        let input = ProducerAuthoritativeInput {
            descriptor,
            advisories: Vec::new(),
            lock_id: None,
            closure_digest: None,
            legacy_producer_bridge: Some(LegacyProducerBridge {
                bridge: compat_manifest,
                origin: LegacyProducerBridgeOrigin::CompatibilityInput,
            }),
            _cleanup: ProducerMaterializationCleanup {
                run_state_dir: dir.path().join("run-state"),
            },
        };

        let error = input
            .validate_legacy_producer_bridge()
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

        assert!(input.descriptor.lock.binding.entries.is_empty());
        assert!(input.descriptor.lock.binding.unresolved.is_empty());
        assert!(input.descriptor.lock.attestations.entries.is_empty());
        assert!(input.descriptor.lock.attestations.unresolved.is_empty());

        let generated_lock = capsule_core::ato_lock::load_unvalidated_from_path(
            &input._cleanup.run_state_dir.join("ato.lock.json"),
        )
        .expect("read generated lock");
        assert!(generated_lock.binding.entries.is_empty());
        assert!(generated_lock.binding.unresolved.is_empty());
        assert!(generated_lock.attestations.entries.is_empty());
        assert!(generated_lock.attestations.unresolved.is_empty());
    }

    #[test]
    fn publish_metadata_from_lock_classifies_source_derived_desktop_delivery() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "delivery".to_string(),
            serde_json::json!({
                "mode": "source-derivation",
                "artifact": {
                    "kind": "desktop-native",
                    "provenance_limited": false
                }
            }),
        );

        let metadata = publish_metadata_from_lock(&lock).expect("publish metadata");
        assert_eq!(
            metadata.identity_class,
            PublishArtifactIdentityClass::SourceDerivedUnsignedBundle
        );
        assert_eq!(metadata.delivery_mode.as_deref(), Some("source-derivation"));
        assert!(!metadata.provenance_limited);
    }

    #[test]
    fn publish_metadata_from_lock_classifies_imported_desktop_delivery() {
        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "delivery".to_string(),
            serde_json::json!({
                "mode": "artifact-import",
                "artifact": {
                    "kind": "desktop-native",
                    "provenance_limited": true
                }
            }),
        );

        let metadata = publish_metadata_from_lock(&lock).expect("publish metadata");
        assert_eq!(
            metadata.identity_class,
            PublishArtifactIdentityClass::ImportedThirdPartyArtifact
        );
        assert_eq!(metadata.delivery_mode.as_deref(), Some("artifact-import"));
        assert!(metadata.provenance_limited);
    }

    #[test]
    fn compatibility_accessors_ignore_lock_derived_legacy_bridge() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"demo","version":"0.1.0","scripts":{"start":"node index.js"}}"#,
        )
        .expect("package.json");
        std::fs::write(
            dir.path().join("package-lock.json"),
            r#"{"name":"demo","version":"0.1.0","lockfileVersion":3,"packages":{}}"#,
        )
        .expect("package-lock.json");
        std::fs::write(dir.path().join("index.js"), "console.log('demo');\n").expect("index.js");

        let input = resolve_producer_authoritative_input(
            dir.path(),
            Arc::new(crate::reporters::CliReporter::new(false)),
            true,
        )
        .expect("resolve producer input");

        assert!(input.legacy_producer_manifest_value().is_some());
        assert!(input.compatibility_input_repository().is_none());
        assert!(input.compatibility_publish_registry().is_none());
        assert!(!input.compatibility_store_playground_enabled());
    }

    #[test]
    fn compatibility_accessors_only_read_explicit_compatibility_input() {
        let manifest_raw = r#"
schema_version = "0.3"
name = "demo-app"
version = "1.2.3"
type = "app"

runtime = "source/deno"
run = "main.ts"
[metadata]
repository = "https://github.com/example/demo-app"

[store]
registry = "https://registry.example.test"
playground = true
"#;
        let dir = tempdir().expect("tempdir");
        let manifest_path = dir.path().join("generated.capsule.toml");
        std::fs::write(&manifest_path, manifest_raw).expect("write manifest");

        let mut lock = AtoLock::default();
        lock.contract.entries.insert(
            "metadata".to_string(),
            json!({"name": "demo-app", "version": "1.2.3", "default_target": "cli"}),
        );
        lock.contract.entries.insert(
            "process".to_string(),
            json!({"entrypoint": "main.ts", "driver": "deno"}),
        );
        lock.resolution.entries.insert(
            "runtime".to_string(),
            json!({"kind": "deno", "selected_target": "cli"}),
        );
        lock.resolution.entries.insert(
            "resolved_targets".to_string(),
            json!([{"label": "cli", "runtime": "source", "driver": "deno", "entrypoint": "main.ts"}]),
        );
        lock.resolution
            .entries
            .insert("closure".to_string(), json!({"kind": "metadata_only"}));
        let descriptor = capsule_core::router::route_lock(
            &manifest_path,
            &lock,
            dir.path(),
            capsule_core::router::ExecutionProfile::Release,
            None,
        )
        .expect("route lock")
        .plan;
        let bridge = CompatManifestBridge::from_manifest_value(
            &toml::from_str(manifest_raw).expect("manifest value"),
        )
        .expect("compat bridge");

        let input = ProducerAuthoritativeInput {
            descriptor,
            advisories: Vec::new(),
            lock_id: None,
            closure_digest: None,
            legacy_producer_bridge: Some(LegacyProducerBridge {
                bridge,
                origin: LegacyProducerBridgeOrigin::CompatibilityInput,
            }),
            _cleanup: ProducerMaterializationCleanup {
                run_state_dir: dir.path().join("run-state"),
            },
        };

        assert_eq!(
            input.compatibility_input_repository().as_deref(),
            Some("https://github.com/example/demo-app")
        );
        assert_eq!(
            input.compatibility_publish_registry().as_deref(),
            Some("https://registry.example.test")
        );
        assert!(input.compatibility_store_playground_enabled());
    }
}
