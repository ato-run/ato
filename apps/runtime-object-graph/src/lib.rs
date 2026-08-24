//! Shared application boundary for downloading and independently validating
//! content-addressed Capsule object graphs.
//!
//! The Validator Agent, Connected Realization Worker, and CLI all call this
//! implementation. Transport declarations are checked only after semantic
//! references have been re-derived from decoded object content.

#![forbid(unsafe_code)]

mod validator_agent;

pub use validator_agent::{
    HttpValidatorApi, ValidatorAgent, ValidatorAgentConfig, ValidatorRunOutcome,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ato_adapter_workspace::WorkspaceSnapshot;
use ato_compose::{COMPOSE_SEMANTICS_ID, ComposeReferences, decode_composite_residual};
use ato_computation::{ComputationRef, ContentRef, ResolvedComputation, SemanticsId};
use ato_materializer_replay::{ReplayReferences, ReplayV2References};
use ato_materializer_snapshot::{SnapshotReferences, WorkspaceSnapshotReferences};
use ato_materializer_vm_snapshot::{
    VM_SNAPSHOT_MATERIALIZER_ID, VmSnapshotDescriptor, VmSnapshotReferences,
};
use ato_objects::{
    BundleError, ComputationReferences, FsObjectStore, GraphMaterialization, GraphObjectDescriptor,
    ObjectGraphClosure, ObjectLink, ObjectResolver, ObjectStore, ReferenceRegistry,
    export_object_graph, read_exact_object, resolve_computation, verify_declared_object_graph,
};
use ato_record_writer::verify_frontier_object;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

pub const OBJECT_GRAPH_INDEX_VERSION: u32 = 1;
pub const MAX_RUNTIME_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_RUNTIME_GRAPH_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MAX_RUNTIME_GRAPH_OBJECTS: usize = 10_000;
const AUTHORING_SEMANTICS_ID: &str = "ato.authoring@1";
const MAX_AUTHORING_STATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKSPACE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityPolicy {
    Private,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedPort {
    pub port_id: String,
    pub protocol: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredBinding {
    pub id: String,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectGraphIndexV1 {
    pub version: u32,
    pub root_computation_ref: String,
    pub objects: Vec<GraphObjectDescriptor>,
    pub materializations: Vec<GraphMaterialization>,
    pub exported_ports: Vec<ExportedPort>,
    pub required_bindings: Vec<RequiredBinding>,
    pub visibility_policy: VisibilityPolicy,
}

impl ObjectGraphIndexV1 {
    pub fn new(
        closure: ObjectGraphClosure,
        mut exported_ports: Vec<ExportedPort>,
        mut required_bindings: Vec<RequiredBinding>,
        visibility_policy: VisibilityPolicy,
    ) -> Self {
        exported_ports.sort();
        required_bindings.sort();
        Self {
            version: OBJECT_GRAPH_INDEX_VERSION,
            root_computation_ref: closure.root_computation_ref,
            objects: closure.objects,
            materializations: closure.materializations,
            exported_ports,
            required_bindings,
            visibility_policy,
        }
    }

    pub fn digest(&self) -> Result<String> {
        Ok(content_digest(&serde_jcs::to_vec(self)?))
    }

    pub fn logical_bytes(&self) -> Result<u64> {
        self.objects.iter().try_fold(0_u64, |total, object| {
            total
                .checked_add(object.size_bytes)
                .context("object graph logical byte count overflow")
        })
    }
}

/// Builds the transport index from decoded semantic content and validates the
/// exact graph before any upload is attempted. Callers never supply cached
/// Port or Binding summaries.
pub fn build_runtime_object_graph_index(
    root: &ComputationRef,
    materializations: &[GraphMaterialization],
    objects: &dyn ObjectStore,
    references: &ReferenceRegistry,
    visibility_policy: VisibilityPolicy,
) -> Result<ObjectGraphIndexV1> {
    let closure = export_object_graph(root, materializations, objects, references)?;
    let summary = derive_runtime_summary(root, objects)?;
    let index = ObjectGraphIndexV1::new(
        closure,
        summary.exported_ports,
        summary.required_bindings,
        visibility_policy,
    );
    validate_runtime_object_graph(&index, objects, references)?;
    Ok(index)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphDownloadExpectation {
    pub index_digest: String,
    pub root_computation_ref: String,
    pub object_count: usize,
    pub logical_bytes: u64,
}

pub trait RuntimeGraphSource: Send + Sync {
    fn load_index(&self) -> Result<Vec<u8>>;
    fn load_object(&self, reference: &ContentRef, expected_size: u64) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeGraphValidationReport {
    pub format_version: u32,
    pub bundle_index_digest: String,
    pub root_computation_ref: String,
    pub materializations: Vec<GraphMaterialization>,
    pub exported_ports: Vec<ExportedPort>,
    pub required_bindings: Vec<RequiredBinding>,
    pub workspace_file_count: usize,
    pub object_count: usize,
    pub decoded_size: u64,
    pub validation: ValidationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationStatus {
    pub status: String,
}

pub struct ValidatedRuntimeGraph {
    index: ObjectGraphIndexV1,
    objects: FsObjectStore,
    report: RuntimeGraphValidationReport,
    _session: TempDir,
}

impl ValidatedRuntimeGraph {
    pub fn index(&self) -> &ObjectGraphIndexV1 {
        &self.index
    }

    pub fn objects(&self) -> &FsObjectStore {
        &self.objects
    }

    pub fn report(&self) -> &RuntimeGraphValidationReport {
        &self.report
    }
}

pub fn download_and_validate_graph(
    source: &dyn RuntimeGraphSource,
    expectation: &GraphDownloadExpectation,
    work_root: &Path,
) -> Result<ValidatedRuntimeGraph> {
    fs::create_dir_all(work_root)
        .with_context(|| format!("failed to create graph work root {}", work_root.display()))?;
    let session = tempfile::Builder::new()
        .prefix("runtime-object-graph-")
        .tempdir_in(work_root)
        .context("failed to create isolated graph validation directory")?;
    let index_bytes = source
        .load_index()
        .context("failed to download graph index")?;
    let index = decode_and_verify_index(&index_bytes, expectation)?;
    let objects = FsObjectStore::open(session.path().join("objects"))?;
    for descriptor in &index.objects {
        let reference = ContentRef::parse(&descriptor.content_ref)
            .with_context(|| format!("invalid object reference {}", descriptor.content_ref))?;
        let bytes = source
            .load_object(&reference, descriptor.size_bytes)
            .with_context(|| format!("failed to download object {reference}"))?;
        if bytes.len() as u64 != descriptor.size_bytes {
            bail!("downloaded object {reference} length mismatch");
        }
        objects
            .insert(&reference, &bytes)
            .with_context(|| format!("downloaded object {reference} digest mismatch"))?;
    }
    let report = validate_runtime_object_graph(&index, &objects, &standard_reference_registry()?)?;
    Ok(ValidatedRuntimeGraph {
        index,
        objects,
        report,
        _session: session,
    })
}

pub fn decode_and_verify_index(
    bytes: &[u8],
    expectation: &GraphDownloadExpectation,
) -> Result<ObjectGraphIndexV1> {
    if content_digest(bytes) != expectation.index_digest {
        bail!("object graph index digest mismatch");
    }
    let index: ObjectGraphIndexV1 =
        serde_json::from_slice(bytes).context("object graph index is malformed")?;
    if index.version != OBJECT_GRAPH_INDEX_VERSION {
        bail!("unsupported object graph index version {}", index.version);
    }
    if serde_jcs::to_vec(&index)? != bytes {
        bail!("object graph index is not canonical JCS");
    }
    validate_index_shape(&index)?;
    if index.root_computation_ref != expectation.root_computation_ref
        || index.objects.len() != expectation.object_count
        || index.logical_bytes()? != expectation.logical_bytes
    {
        bail!("object graph index does not match claimed job identity");
    }
    Ok(index)
}

pub fn validate_runtime_object_graph(
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
) -> Result<RuntimeGraphValidationReport> {
    validate_index_shape(index)?;
    let root = ComputationRef::parse(&index.root_computation_ref)?;
    let declared = ObjectGraphClosure {
        root_computation_ref: index.root_computation_ref.clone(),
        objects: index.objects.clone(),
        materializations: index.materializations.clone(),
    };
    let derived = verify_declared_object_graph(&declared, objects, references)
        .context("decoded semantic closure differs from declared object graph")?;
    if derived.root_computation_ref != root.to_string() {
        bail!("decoded object graph changed root ComputationRef");
    }

    let summary = derive_runtime_summary(&root, objects)?;
    if summary.exported_ports != index.exported_ports {
        bail!("declared exported Ports differ from decoded Computation boundary");
    }
    if summary.required_bindings != index.required_bindings {
        bail!("declared required Bindings differ from decoded Computation residual");
    }

    if let Some((_, frontier)) = vm_capture_refs(index, objects)? {
        verify_frontier_object(&frontier, objects)
            .context("VM materialization RecordFrontier closure is invalid")?;
    }

    Ok(RuntimeGraphValidationReport {
        format_version: 2,
        bundle_index_digest: index.digest()?,
        root_computation_ref: root.to_string(),
        materializations: derived.materializations,
        exported_ports: summary.exported_ports,
        required_bindings: summary.required_bindings,
        workspace_file_count: summary.workspace_file_count,
        object_count: derived.objects.len(),
        decoded_size: index.logical_bytes()?,
        validation: ValidationStatus {
            status: "valid".to_owned(),
        },
    })
}

pub fn vm_capture_refs(
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
) -> Result<Option<(ContentRef, ContentRef)>> {
    let Some(materialization) = index
        .materializations
        .iter()
        .find(|item| item.id == VM_SNAPSHOT_MATERIALIZER_ID)
    else {
        return Ok(None);
    };
    let descriptor_ref = ContentRef::parse(&materialization.descriptor_ref)?;
    let declared = index
        .objects
        .iter()
        .find(|item| item.content_ref == materialization.descriptor_ref)
        .context("VM materialization descriptor is absent from object closure")?;
    let bytes = read_exact_object(
        objects,
        &descriptor_ref,
        declared.size_bytes,
        MAX_RUNTIME_OBJECT_BYTES,
    )?;
    let descriptor: VmSnapshotDescriptor =
        serde_json::from_slice(&bytes).context("VM materialization descriptor is malformed")?;
    if serde_jcs::to_vec(&descriptor)? != bytes {
        bail!("VM materialization descriptor is not canonical JCS");
    }
    if descriptor.target_computation_ref != index.root_computation_ref {
        bail!("VM materialization target does not match graph root ComputationRef");
    }
    let frontier = ContentRef::parse(
        descriptor
            .record_frontier_ref
            .as_deref()
            .context("VM materialization descriptor omitted RecordFrontier")?,
    )?;
    Ok(Some((descriptor_ref, frontier)))
}

pub fn standard_reference_registry() -> Result<ReferenceRegistry> {
    let mut registry = ReferenceRegistry::default();
    registry.register(Arc::new(AuthoringReferences::new()))?;
    registry.register(Arc::new(ComposeReferences::default()))?;
    registry.register_materializer(Arc::new(ReplayReferences))?;
    registry.register_materializer(Arc::new(ReplayV2References))?;
    registry.register_materializer(Arc::new(SnapshotReferences))?;
    registry.register_materializer(Arc::new(WorkspaceSnapshotReferences))?;
    registry.register_materializer(Arc::new(VmSnapshotReferences))?;
    Ok(registry)
}

fn validate_index_shape(index: &ObjectGraphIndexV1) -> Result<()> {
    if index.objects.is_empty() || index.objects.len() > MAX_RUNTIME_GRAPH_OBJECTS {
        bail!("object graph object count is outside permitted bounds");
    }
    let mut seen = BTreeSet::new();
    for descriptor in &index.objects {
        ContentRef::parse(&descriptor.content_ref)?;
        if descriptor.size_bytes == 0 || descriptor.size_bytes > MAX_RUNTIME_OBJECT_BYTES {
            bail!("object {} has an invalid size", descriptor.content_ref);
        }
        if !seen.insert(descriptor.content_ref.as_str()) {
            bail!("object graph contains a duplicate descriptor");
        }
        let references = descriptor.references.iter().collect::<BTreeSet<_>>();
        if references.len() != descriptor.references.len() {
            bail!("object graph contains duplicate declared references");
        }
    }
    if index.logical_bytes()? > MAX_RUNTIME_GRAPH_BYTES {
        bail!("object graph decoded closure is too large");
    }
    if !seen.contains(index.root_computation_ref.as_str()) {
        bail!("object graph root is not declared");
    }
    for descriptor in &index.objects {
        for reference in &descriptor.references {
            if !seen.contains(reference.as_str()) {
                bail!("object graph declared reference is missing");
            }
        }
    }
    Ok(())
}

fn content_digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

#[derive(Debug, Deserialize)]
struct AuthoringStateProjection {
    version: u32,
    config: AuthoringConfigProjection,
    workspace_snapshot: String,
}

#[derive(Debug, Deserialize)]
struct AuthoringConfigProjection {
    #[serde(default)]
    binding: Vec<BindingProjection>,
}

#[derive(Debug, Deserialize)]
struct BindingProjection {
    id: String,
    protocol: String,
}

fn load_authoring_state(
    computation: &ResolvedComputation,
    objects: &dyn ObjectResolver,
) -> Result<AuthoringStateProjection> {
    let residual = &computation.object().residual;
    let metadata = objects.metadata(residual)?;
    let bytes = read_exact_object(objects, residual, metadata.size, MAX_AUTHORING_STATE_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&value)? != bytes {
        bail!("authoring state is not canonical JCS");
    }
    let state: AuthoringStateProjection = serde_json::from_value(value)?;
    if state.version != 1 {
        bail!("unsupported authoring state version {}", state.version);
    }
    Ok(state)
}

fn load_workspace_snapshot(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<WorkspaceSnapshot> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(
        objects,
        reference,
        metadata.size,
        MAX_WORKSPACE_SNAPSHOT_BYTES,
    )?;
    let snapshot: WorkspaceSnapshot = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&snapshot)? != bytes {
        bail!("workspace snapshot is not canonical JCS");
    }
    Ok(snapshot)
}

struct RuntimeSummary {
    exported_ports: Vec<ExportedPort>,
    required_bindings: Vec<RequiredBinding>,
    workspace_file_count: usize,
}

fn derive_runtime_summary(
    root: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<RuntimeSummary> {
    let resolved = resolve_computation(objects, root)?;
    let mut exported_ports = resolved
        .object()
        .boundary
        .iter()
        .map(|(id, definition)| ExportedPort {
            port_id: id.to_string(),
            protocol: definition.protocol.to_string(),
            role: definition.role.to_string(),
        })
        .collect::<Vec<_>>();
    exported_ports.sort();

    let mut bindings = BTreeSet::new();
    let mut workspaces = BTreeMap::new();
    collect_runtime_projection(root, objects, &mut bindings, &mut workspaces)?;
    Ok(RuntimeSummary {
        exported_ports,
        required_bindings: bindings.into_iter().collect(),
        workspace_file_count: workspaces.len(),
    })
}

fn collect_runtime_projection(
    computation: &ComputationRef,
    objects: &dyn ObjectResolver,
    bindings: &mut BTreeSet<RequiredBinding>,
    workspaces: &mut BTreeMap<String, String>,
) -> Result<()> {
    let resolved = resolve_computation(objects, computation)?;
    let authoring = SemanticsId::parse(AUTHORING_SEMANTICS_ID)?;
    if resolved.object().semantics == authoring {
        let state = load_authoring_state(&resolved, objects)?;
        bindings.extend(
            state
                .config
                .binding
                .into_iter()
                .map(|binding| RequiredBinding {
                    id: binding.id,
                    schema: binding.protocol,
                }),
        );
        let snapshot_ref = ContentRef::parse(&state.workspace_snapshot)?;
        workspaces.extend(load_workspace_snapshot(&snapshot_ref, objects)?.files);
        return Ok(());
    }
    if resolved.object().semantics == SemanticsId::parse(COMPOSE_SEMANTICS_ID)? {
        let residual = &resolved.object().residual;
        let metadata = objects.metadata(residual)?;
        let bytes = read_exact_object(objects, residual, metadata.size, 16 * 1024 * 1024)?;
        let composite = decode_composite_residual(&bytes)?;
        for child in composite.nodes.values() {
            collect_runtime_projection(child, objects, bindings, workspaces)?;
        }
        return Ok(());
    }
    bail!(
        "Computation {} has no registered runtime summary projection",
        computation
    )
}

struct AuthoringReferences {
    semantics: SemanticsId,
}

impl AuthoringReferences {
    fn new() -> Self {
        Self {
            semantics: SemanticsId::parse(AUTHORING_SEMANTICS_ID)
                .expect("valid static authoring semantics id"),
        }
    }
}

impl ComputationReferences for AuthoringReferences {
    fn semantics(&self) -> &SemanticsId {
        &self.semantics
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let state = load_authoring_state(computation, objects).map_err(storage_bundle_error)?;
        let snapshot_ref = ContentRef::parse(&state.workspace_snapshot).map_err(|error| {
            BundleError::InvalidReference {
                value: state.workspace_snapshot.clone(),
                reason: error.to_string(),
            }
        })?;
        let snapshot =
            load_workspace_snapshot(&snapshot_ref, objects).map_err(storage_bundle_error)?;
        let mut links = vec![ObjectLink::Content(snapshot_ref)];
        for reference in snapshot.files.into_values() {
            links.push(ObjectLink::Content(ContentRef::parse(&reference).map_err(
                |error| BundleError::InvalidReference {
                    value: reference,
                    reason: error.to_string(),
                },
            )?));
        }
        Ok(links)
    }
}

fn storage_bundle_error(error: anyhow::Error) -> BundleError {
    BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
}

#[cfg(test)]
mod tests;
