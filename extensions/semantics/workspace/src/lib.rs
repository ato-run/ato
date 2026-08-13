//! `ato.workspace@1` is the concrete semantics of an executable development
//! workspace. Repository syntax and physical process execution remain outside
//! this crate.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use ato_computation::{ComputationObject, ContentRef, ResolvedComputation, SemanticsId};
use ato_kernel::{Action, SemanticError, SemanticHost, SemanticStep, Semantics};
use ato_objects::{
    BundleError, ComputationReferences, ObjectLink, ObjectResolver, read_exact_object,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const WORKSPACE_SEMANTICS_ID: &str = "ato.workspace@1";
pub const MAX_WORKSPACE_RESIDUAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceResidual {
    pub source: String,
    pub toolchain: ToolchainConstraint,
    pub package_manager: Option<String>,
    pub entrypoint: Vec<String>,
    pub working_directory: String,
    pub environment: BTreeMap<String, String>,
    pub secret_bindings: BTreeMap<String, String>,
    pub realization: RealizationConstraint,
    pub phase: WorkspacePhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClosure {
    pub entries: Vec<SourceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEntry {
    pub path: String,
    pub content: String,
    pub executable: bool,
    pub kind: SourceEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEntryKind {
    File,
    Symlink,
}

pub struct WorkspaceReferences {
    id: SemanticsId,
}

impl Default for WorkspaceReferences {
    fn default() -> Self {
        Self {
            id: SemanticsId::parse(WORKSPACE_SEMANTICS_ID)
                .expect("static workspace semantics id is valid"),
        }
    }
}

impl ComputationReferences for WorkspaceReferences {
    fn semantics(&self) -> &SemanticsId {
        &self.id
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let residual_ref = &computation.object().residual;
        let residual_metadata = objects.metadata(residual_ref)?;
        let residual_bytes = read_exact_object(
            objects,
            residual_ref,
            residual_metadata.size,
            MAX_WORKSPACE_RESIDUAL_BYTES,
        )?;
        let residual = decode_workspace_residual(&residual_bytes).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        let source = ContentRef::parse(&residual.source).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        let source_metadata = objects.metadata(&source)?;
        let source_bytes = read_exact_object(
            objects,
            &source,
            source_metadata.size,
            MAX_WORKSPACE_RESIDUAL_BYTES,
        )?;
        let closure: SourceClosure = serde_json::from_slice(&source_bytes)?;
        let mut links = vec![ObjectLink::Content(source)];
        for entry in closure.entries {
            let reference = ContentRef::parse(entry.content).map_err(|error| {
                BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
            })?;
            links.push(ObjectLink::Content(reference));
        }
        Ok(links)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainConstraint {
    pub family: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationConstraint {
    pub network_allow: Vec<String>,
    pub writable_paths: Vec<String>,
    pub sandbox_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspacePhase {
    Ready,
    Exited { code: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceOutcome {
    pub exit_code: i32,
}

pub trait WorkspaceProvider: Send + Sync {
    fn realize(&self, workspace: &WorkspaceResidual) -> Result<WorkspaceOutcome, ProviderError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("workspace realization failed: {message}")]
pub struct ProviderError {
    message: String,
}

impl ProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceCodecError {
    #[error("workspace residual is {actual} bytes; maximum is {maximum}")]
    ObjectTooLarge { actual: u64, maximum: u64 },
    #[error("workspace residual JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workspace residual is not canonical JCS")]
    NonCanonical,
    #[error("invalid workspace residual: {0}")]
    Invalid(String),
}

pub fn encode_workspace_residual(
    residual: &WorkspaceResidual,
) -> Result<Vec<u8>, WorkspaceCodecError> {
    validate_residual(residual)?;
    let bytes = serde_jcs::to_vec(residual)?;
    ensure_size(&bytes)?;
    Ok(bytes)
}

pub fn decode_workspace_residual(bytes: &[u8]) -> Result<WorkspaceResidual, WorkspaceCodecError> {
    ensure_size(bytes)?;
    let residual: WorkspaceResidual = serde_json::from_slice(bytes)?;
    validate_residual(&residual)?;
    if encode_workspace_residual(&residual)? != bytes {
        return Err(WorkspaceCodecError::NonCanonical);
    }
    Ok(residual)
}

fn ensure_size(bytes: &[u8]) -> Result<(), WorkspaceCodecError> {
    if bytes.len() as u64 > MAX_WORKSPACE_RESIDUAL_BYTES {
        return Err(WorkspaceCodecError::ObjectTooLarge {
            actual: bytes.len() as u64,
            maximum: MAX_WORKSPACE_RESIDUAL_BYTES,
        });
    }
    Ok(())
}

fn validate_residual(residual: &WorkspaceResidual) -> Result<(), WorkspaceCodecError> {
    ato_computation::ContentRef::parse(&residual.source)
        .map_err(|error| WorkspaceCodecError::Invalid(error.to_string()))?;
    if residual.toolchain.family.is_empty() {
        return Err(WorkspaceCodecError::Invalid(
            "toolchain family must not be empty".to_owned(),
        ));
    }
    if matches!(residual.phase, WorkspacePhase::Ready) && residual.entrypoint.is_empty() {
        return Err(WorkspaceCodecError::Invalid(
            "ready workspace entrypoint must not be empty".to_owned(),
        ));
    }
    ensure_sorted_unique("network_allow", &residual.realization.network_allow)?;
    ensure_sorted_unique("writable_paths", &residual.realization.writable_paths)?;
    Ok(())
}

fn ensure_sorted_unique(field: &str, values: &[String]) -> Result<(), WorkspaceCodecError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(WorkspaceCodecError::Invalid(format!(
            "{field} must be sorted and unique"
        )));
    }
    Ok(())
}

pub struct WorkspaceSemantics {
    id: SemanticsId,
    provider: Arc<dyn WorkspaceProvider>,
}

impl WorkspaceSemantics {
    pub fn new(provider: Arc<dyn WorkspaceProvider>) -> Self {
        Self {
            id: SemanticsId::parse(WORKSPACE_SEMANTICS_ID)
                .expect("static workspace semantics id is valid"),
            provider,
        }
    }
}

impl<V> Semantics<V> for WorkspaceSemantics
where
    V: Clone + Send + Sync + 'static,
{
    fn id(&self) -> &SemanticsId {
        &self.id
    }

    fn validate(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<V>,
    ) -> Result<(), SemanticError> {
        load(current, host).map(|_| ())
    }

    fn enabled(
        &self,
        current: &ResolvedComputation,
        host: &dyn SemanticHost<V>,
    ) -> Result<Vec<Action<V>>, SemanticError> {
        Ok(match load(current, host)?.phase {
            WorkspacePhase::Ready => vec![Action::Tau],
            WorkspacePhase::Exited { .. } => Vec::new(),
        })
    }

    fn step(
        &self,
        current: &ResolvedComputation,
        action: &Action<V>,
        host: &dyn SemanticHost<V>,
    ) -> Result<SemanticStep<V>, SemanticError> {
        if !matches!(action, Action::Tau) {
            return Err(SemanticError::new(
                "workspace realization is an internal transition",
            ));
        }
        let mut residual = load(current, host)?;
        if !matches!(residual.phase, WorkspacePhase::Ready) {
            return Err(SemanticError::new("workspace has already exited"));
        }
        let outcome = self
            .provider
            .realize(&residual)
            .map_err(|error| SemanticError::new(error.to_string()))?;
        residual.phase = WorkspacePhase::Exited {
            code: outcome.exit_code,
        };
        let bytes = encode_workspace_residual(&residual)
            .map_err(|error| SemanticError::new(error.to_string()))?;
        let residual = host
            .put_object(&bytes)
            .map_err(|error| SemanticError::new(error.to_string()))?;
        Ok(SemanticStep {
            action: Action::Tau,
            successor: ComputationObject {
                semantics: current.object().semantics.clone(),
                boundary: current.object().boundary.clone(),
                residual,
            },
        })
    }
}

pub fn observe_exit(residual: &WorkspaceResidual) -> Option<i32> {
    match residual.phase {
        WorkspacePhase::Ready => None,
        WorkspacePhase::Exited { code } => Some(code),
    }
}

fn load<V>(
    current: &ResolvedComputation,
    host: &dyn SemanticHost<V>,
) -> Result<WorkspaceResidual, SemanticError> {
    if current.object().semantics.as_str() != WORKSPACE_SEMANTICS_ID {
        return Err(SemanticError::new("wrong workspace semantics id"));
    }
    let bytes = host
        .get_object(&current.object().residual, MAX_WORKSPACE_RESIDUAL_BYTES)
        .map_err(|error| SemanticError::new(error.to_string()))?;
    decode_workspace_residual(&bytes).map_err(|error| SemanticError::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> WorkspaceResidual {
        WorkspaceResidual {
            source: format!("blake3:{}", "ab".repeat(32)),
            toolchain: ToolchainConstraint {
                family: "python".to_owned(),
                version: Some("3.12".to_owned()),
            },
            package_manager: Some("pip".to_owned()),
            entrypoint: vec!["python".to_owned(), "main.py".to_owned()],
            working_directory: ".".to_owned(),
            environment: BTreeMap::new(),
            secret_bindings: BTreeMap::from([(
                "API_TOKEN".to_owned(),
                "secret://example/api-token".to_owned(),
            )]),
            realization: RealizationConstraint {
                network_allow: vec!["pypi.org".to_owned()],
                writable_paths: vec![".cache".to_owned()],
                sandbox_required: true,
            },
            phase: WorkspacePhase::Ready,
        }
    }

    #[test]
    fn residual_roundtrip_is_canonical_and_keeps_secret_values_out() {
        let bytes = encode_workspace_residual(&fixture()).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();

        assert!(!text.contains("actual-secret"));
        assert_eq!(decode_workspace_residual(&bytes).unwrap(), fixture());
    }

    #[test]
    fn runtime_version_changes_semantic_residual() {
        let python_312 = encode_workspace_residual(&fixture()).unwrap();
        let mut other = fixture();
        other.toolchain.version = Some("3.11".to_owned());
        let python_311 = encode_workspace_residual(&other).unwrap();

        assert_ne!(python_312, python_311);
    }
}
