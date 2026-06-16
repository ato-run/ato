//! Pure request/response boundary for future artifact build producers.
//!
//! This module does not submit work or run a worker. It fixes the build-safe
//! contract that a future remote producer may consume without crossing launch,
//! install-instance, secret, or persistent-state boundaries.

#![allow(dead_code)] // The producer implementation follows this contract slice.

use std::path::{Component, Path};

use anyhow::{Context, Result};
use blake3::Hasher;
use serde::{Deserialize, Serialize};

use crate::application::build_materialization::BuildObservation;
use crate::application::phase_materializer::{
    BuildOutputLayerRecord, MATERIALIZER_SCHEMA_VERSION, PROJECTION_ALGORITHM_VERSION,
    build_output_contract_for_observation, materialization_key_for_observation,
};

const PRODUCER_REQUEST_SCHEMA_VERSION: &str = "ato-artifact-build-producer-request-v0";
const ARTIFACT_BUILD_ID_VERSION: &str = "ato-artifact-build-id-v0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactBuildProducerRequest {
    pub(crate) schema_version: String,
    /// Canonical reusable build artifact cache identity.
    pub(crate) artifact_build_id: String,
    /// Current phase materialization compatibility key.
    pub(crate) materialization_key: String,
    pub(crate) source: ArtifactBuildSourceRef,
    pub(crate) source_tree_hash: String,
    pub(crate) recipe_digest: String,
    pub(crate) lock_digest: Option<String>,
    pub(crate) target_label: String,
    pub(crate) phase: ArtifactBuildPhase,
    pub(crate) build_command_identity: String,
    pub(crate) output_contract_digest: String,
    pub(crate) outputs: Vec<String>,
    pub(crate) platform_profile: ArtifactPlatformProfile,
    pub(crate) toolchain_identity: String,
    pub(crate) materializer_schema_version: String,
    pub(crate) projection_algorithm_version: String,
    pub(crate) policy: ArtifactBuildProducerPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactBuildProducerPolicy {
    pub(crate) allow_network: bool,
    pub(crate) allowed_env_keys: Vec<String>,
    pub(crate) disallow_secret_values: bool,
    pub(crate) run_phase_allowed: bool,
    pub(crate) persistent_state_allowed: bool,
}

impl ArtifactBuildProducerPolicy {
    fn build_output_v0() -> Self {
        Self {
            allow_network: false,
            allowed_env_keys: Vec::new(),
            disallow_secret_values: true,
            run_phase_allowed: false,
            persistent_state_allowed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum ArtifactBuildSourceRef {
    PublicGitHubCommit {
        repo: String,
        commit: String,
    },
    SourceSnapshot {
        source_tree_hash: String,
        snapshot_ref: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactBuildPhase {
    Build,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactPlatformProfile {
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) abi: String,
    pub(crate) libc_or_runtime_abi: Option<String>,
    pub(crate) native_addon_boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactBuildProducerResponse {
    pub(crate) schema_version: String,
    pub(crate) artifact_build_id: String,
    pub(crate) materialization_key: String,
    pub(crate) status: ArtifactBuildProducerStatus,
    pub(crate) output_layer: Option<BuildOutputLayerRecord>,
    pub(crate) remote_layer_ref: Option<String>,
    pub(crate) provenance: ArtifactBuildProducerProvenance,
    pub(crate) build_log_ref: Option<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactBuildProducerStatus {
    Produced,
    CacheHit,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactBuildProducerProvenance {
    pub(crate) kind: String,
    pub(crate) producer: String,
}

pub(crate) fn validate_artifact_build_producer_request(
    request: &ArtifactBuildProducerRequest,
) -> Result<()> {
    if request.schema_version != PRODUCER_REQUEST_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported artifact build producer request schema '{}'",
            request.schema_version
        );
    }
    ensure_not_empty("artifact_build_id", &request.artifact_build_id)?;
    ensure_not_empty("materialization_key", &request.materialization_key)?;
    if looks_like_execution_id(&request.artifact_build_id) {
        anyhow::bail!("artifact_build_id must not carry an execution_id identity");
    }
    if !request.artifact_build_id.starts_with("build_") {
        anyhow::bail!(
            "artifact_build_id must start with 'build_', got: {}",
            &request.artifact_build_id
        );
    }
    ensure_not_empty("source_tree_hash", &request.source_tree_hash)?;
    ensure_not_empty("recipe_digest", &request.recipe_digest)?;
    ensure_not_empty("target_label", &request.target_label)?;
    ensure_not_empty("build_command_identity", &request.build_command_identity)?;
    ensure_not_empty("output_contract_digest", &request.output_contract_digest)?;
    ensure_not_empty("toolchain_identity", &request.toolchain_identity)?;
    ensure_not_empty(
        "materializer_schema_version",
        &request.materializer_schema_version,
    )?;
    ensure_not_empty(
        "projection_algorithm_version",
        &request.projection_algorithm_version,
    )?;
    validate_source(&request.source)?;
    validate_outputs(&request.outputs)?;
    validate_platform_profile(&request.platform_profile)?;
    validate_policy(&request.policy)?;
    Ok(())
}

pub(crate) fn artifact_build_request_from_observation(
    observation: &BuildObservation,
    source: ArtifactBuildSourceRef,
    source_tree_hash: String,
    recipe_digest: String,
    lock_digest: Option<String>,
    toolchain_identity: String,
    platform_profile: ArtifactPlatformProfile,
) -> Result<ArtifactBuildProducerRequest> {
    let materialization_key = materialization_key_for_observation(observation)
        .context("failed to compute phase materialization compatibility key")?;
    let (output_contract_digest, outputs) = build_output_contract_for_observation(observation)
        .context("failed to derive build output contract for artifact producer")?;
    let artifact_build_id = artifact_build_id_for_observation(
        observation,
        &source_tree_hash,
        &recipe_digest,
        lock_digest.as_deref(),
        &output_contract_digest,
        &outputs,
        &toolchain_identity,
        &platform_profile,
    );
    let request = ArtifactBuildProducerRequest {
        schema_version: PRODUCER_REQUEST_SCHEMA_VERSION.to_string(),
        artifact_build_id,
        materialization_key,
        source,
        source_tree_hash,
        recipe_digest,
        lock_digest,
        target_label: observation.target.clone(),
        phase: ArtifactBuildPhase::Build,
        build_command_identity: observation.command.clone(),
        output_contract_digest,
        outputs,
        platform_profile,
        toolchain_identity,
        materializer_schema_version: MATERIALIZER_SCHEMA_VERSION.to_string(),
        projection_algorithm_version: PROJECTION_ALGORITHM_VERSION.to_string(),
        policy: ArtifactBuildProducerPolicy::build_output_v0(),
    };
    validate_artifact_build_producer_request(&request)?;
    Ok(request)
}

#[allow(clippy::too_many_arguments)]
fn artifact_build_id_for_observation(
    observation: &BuildObservation,
    source_tree_hash: &str,
    recipe_digest: &str,
    lock_digest: Option<&str>,
    output_contract_digest: &str,
    outputs: &[String],
    toolchain_identity: &str,
    platform_profile: &ArtifactPlatformProfile,
) -> String {
    let mut hasher = Hasher::new();
    update_text(&mut hasher, ARTIFACT_BUILD_ID_VERSION);
    update_text(&mut hasher, "build");
    update_text(&mut hasher, source_tree_hash);
    update_text(&mut hasher, recipe_digest);
    update_text(&mut hasher, lock_digest.unwrap_or(""));
    update_text(&mut hasher, &observation.target);
    update_text(&mut hasher, &observation.command);
    update_text(&mut hasher, &observation.input_digest);
    update_text(&mut hasher, output_contract_digest);
    for output in outputs {
        update_text(&mut hasher, output);
    }
    update_text(&mut hasher, toolchain_identity);
    update_platform_profile(&mut hasher, platform_profile);
    update_text(&mut hasher, MATERIALIZER_SCHEMA_VERSION);
    update_text(&mut hasher, PROJECTION_ALGORITHM_VERSION);
    format!("build_{}", hasher.finalize().to_hex())
}

fn validate_source(source: &ArtifactBuildSourceRef) -> Result<()> {
    match source {
        ArtifactBuildSourceRef::PublicGitHubCommit { repo, commit } => {
            ensure_not_empty("source.repo", repo)?;
            if !is_full_git_commit_hash(commit) {
                anyhow::bail!(
                    "public GitHub artifact builds require a full commit hash, not a branch, tag, or latest ref"
                );
            }
        }
        ArtifactBuildSourceRef::SourceSnapshot { .. } => {
            anyhow::bail!(
                "source snapshots are not supported by the v0 artifact build producer contract"
            );
        }
    }
    Ok(())
}

fn validate_outputs(outputs: &[String]) -> Result<()> {
    if outputs.is_empty() {
        anyhow::bail!("artifact build producer request must declare build outputs");
    }
    for output in outputs {
        if output.trim().is_empty() {
            anyhow::bail!("artifact build producer outputs must not contain empty paths");
        }
        let path = Path::new(output);
        // RootDir is checked explicitly: on Windows `/private/build` is
        // rooted but not `is_absolute()`, yet joining it would replace the
        // base path and escape the workspace.
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::Prefix(_) | Component::RootDir
                )
            })
        {
            anyhow::bail!(
                "artifact build producer output '{}' must stay relative",
                output
            );
        }
    }
    Ok(())
}

fn validate_platform_profile(profile: &ArtifactPlatformProfile) -> Result<()> {
    ensure_not_empty("platform_profile.os", &profile.os)?;
    ensure_not_empty("platform_profile.arch", &profile.arch)?;
    ensure_not_empty("platform_profile.abi", &profile.abi)?;
    Ok(())
}

fn validate_policy(policy: &ArtifactBuildProducerPolicy) -> Result<()> {
    if policy.run_phase_allowed {
        anyhow::bail!("artifact build producers must not receive run-phase permission");
    }
    if policy.persistent_state_allowed {
        anyhow::bail!("artifact build producers must not receive persistent state permission");
    }
    if !policy.disallow_secret_values {
        anyhow::bail!("artifact build producers must reject secret values");
    }
    for key in &policy.allowed_env_keys {
        if key.trim().is_empty() {
            anyhow::bail!("artifact build producer allowed_env_keys must not contain empty keys");
        }
        if key.contains('=') {
            anyhow::bail!(
                "artifact build producer allowed_env_keys must contain keys, not KEY=value pairs"
            );
        }
    }
    Ok(())
}

fn ensure_not_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    Ok(())
}

fn looks_like_execution_id(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    [
        "execution:",
        "execution-id:",
        "execution_id:",
        "exec:",
        "exec_",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn is_full_git_commit_hash(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn update_platform_profile(hasher: &mut Hasher, profile: &ArtifactPlatformProfile) {
    update_text(hasher, &profile.os);
    update_text(hasher, &profile.arch);
    update_text(hasher, &profile.abi);
    update_text(hasher, profile.libc_or_runtime_abi.as_deref().unwrap_or(""));
    update_text(
        hasher,
        profile.native_addon_boundary.as_deref().unwrap_or(""),
    );
}

fn update_text(hasher: &mut Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::build_materialization::BuildSpecSource;

    #[test]
    fn request_accepts_minimal_public_github_commit() {
        validate_artifact_build_producer_request(&minimal_request()).expect("request is valid");
    }

    #[test]
    fn request_rejects_github_branch_ref() {
        let mut request = minimal_request();
        request.source = ArtifactBuildSourceRef::PublicGitHubCommit {
            repo: "ato-run/fixture".to_string(),
            commit: "main".to_string(),
        };

        let error =
            validate_artifact_build_producer_request(&request).expect_err("branch must fail");
        assert!(error.to_string().contains("branch"), "{error:#}");
    }

    #[test]
    fn request_rejects_run_phase_allowed() {
        let mut request = minimal_request();
        request.policy.run_phase_allowed = true;

        let error = validate_artifact_build_producer_request(&request)
            .expect_err("run permission must fail");
        assert!(error.to_string().contains("run-phase"), "{error:#}");
    }

    #[test]
    fn request_rejects_persistent_state_allowed() {
        let mut request = minimal_request();
        request.policy.persistent_state_allowed = true;

        let error = validate_artifact_build_producer_request(&request)
            .expect_err("persistent state must fail");
        assert!(error.to_string().contains("persistent state"), "{error:#}");
    }

    #[test]
    fn request_rejects_secret_value_like_env() {
        let mut request = minimal_request();
        request.policy.allowed_env_keys = vec!["API_KEY=secret".to_string()];

        let error =
            validate_artifact_build_producer_request(&request).expect_err("env value must fail");
        assert!(error.to_string().contains("KEY=value"), "{error:#}");
    }

    #[test]
    fn request_rejects_absolute_output_path() {
        let mut request = minimal_request();
        request.outputs = vec!["/private/build/dist".to_string()];

        let error = validate_artifact_build_producer_request(&request)
            .expect_err("absolute output must fail");
        assert!(error.to_string().contains("relative"), "{error:#}");
    }

    #[test]
    fn request_rejects_parent_traversal_output_path() {
        let mut request = minimal_request();
        request.outputs = vec!["../dist".to_string()];

        let error = validate_artifact_build_producer_request(&request)
            .expect_err("parent traversal must fail");
        assert!(error.to_string().contains("relative"), "{error:#}");
    }

    #[test]
    fn request_artifact_build_id_differs_from_execution_id_concept() {
        let mut request = minimal_request();
        request.artifact_build_id = "execution_id:blake3:launch".to_string();

        let error = validate_artifact_build_producer_request(&request)
            .expect_err("execution ids must fail");
        assert!(error.to_string().contains("execution_id"), "{error:#}");
    }

    #[test]
    fn request_does_not_need_install_profile_key_or_capsule_instance_key() {
        let request = minimal_request();
        let value = serde_json::to_value(request).expect("serialize request");
        let object = value.as_object().expect("request object");

        assert!(!object.contains_key("install_profile_key"));
        assert!(!object.contains_key("capsule_instance_key"));
    }

    #[test]
    fn observation_conversion_does_not_include_workspace_absolute_path() {
        let workspace = "/Users/example/workspaces/private-app";
        let observation = BuildObservation {
            source: BuildSpecSource::Declared,
            command: "npm run build".to_string(),
            input_digest: "blake3:input".to_string(),
            outputs: vec!["dist".to_string()],
            target: "web".to_string(),
            working_dir_relative: workspace.to_string(),
        };

        let request = artifact_build_request_from_observation(
            &observation,
            github_source(),
            "blake3:source".to_string(),
            "blake3:recipe".to_string(),
            Some("blake3:lock".to_string()),
            "node@22".to_string(),
            platform_profile(),
        )
        .expect("convert observation");
        let json = serde_json::to_string(&request).expect("serialize request");

        assert!(!json.contains(workspace), "{json}");
    }

    fn minimal_request() -> ArtifactBuildProducerRequest {
        ArtifactBuildProducerRequest {
            schema_version: PRODUCER_REQUEST_SCHEMA_VERSION.to_string(),
            artifact_build_id: "build_abc123artifactabc123artifactabc123artifactabc123artifactabc1"
                .to_string(),
            materialization_key: "blake3:materialization".to_string(),
            source: github_source(),
            source_tree_hash: "blake3:source".to_string(),
            recipe_digest: "blake3:recipe".to_string(),
            lock_digest: Some("blake3:lock".to_string()),
            target_label: "web".to_string(),
            phase: ArtifactBuildPhase::Build,
            build_command_identity: "npm run build".to_string(),
            output_contract_digest: "blake3:output".to_string(),
            outputs: vec!["dist".to_string()],
            platform_profile: platform_profile(),
            toolchain_identity: "node@22".to_string(),
            materializer_schema_version: MATERIALIZER_SCHEMA_VERSION.to_string(),
            projection_algorithm_version: PROJECTION_ALGORITHM_VERSION.to_string(),
            policy: ArtifactBuildProducerPolicy::build_output_v0(),
        }
    }

    fn github_source() -> ArtifactBuildSourceRef {
        ArtifactBuildSourceRef::PublicGitHubCommit {
            repo: "ato-run/fixture".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
        }
    }

    fn platform_profile() -> ArtifactPlatformProfile {
        ArtifactPlatformProfile {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            abi: "gnu".to_string(),
            libc_or_runtime_abi: Some("glibc".to_string()),
            native_addon_boundary: Some("node-api-10".to_string()),
            display: Some("linux-x86_64-gnu".to_string()),
        }
    }
}
