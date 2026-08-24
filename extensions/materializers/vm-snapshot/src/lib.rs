//! Portable, computation-preserving VM Snapshot Materialization.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    Compatibility, ContractDescriptor, MaterializationPathKind, Materializer, MaterializerContext,
    MaterializerError, Realization, RestoreCapability, RunnerCapabilities,
};
use ato_objects::{
    BundleError, MaterializationReferences, ObjectLink, ObjectResolver, read_exact_object,
    resolve_computation,
};
use semver::Version;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use thiserror::Error;

mod firecracker;

pub use firecracker::{
    ActiveFirecrackerCaptureSpec, ActiveFirecrackerRealization, ActiveVmCaptureSource,
    FirecrackerActiveVmCaptureSource, FirecrackerBackend, FirecrackerBackendConfig,
    FirecrackerRecordCaptureBarrier, FirecrackerRecordCaptureLease, FirecrackerRestoreLayout,
};

pub const VM_SNAPSHOT_MATERIALIZER_ID: &str = "ato.materialize.vm.snapshot@1";
pub const VM_SNAPSHOT_DESCRIPTOR_VERSION: u32 = 1;
const MAX_DESCRIPTOR_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FRONTIER_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 32;
const MAX_CHUNKS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Memory,
    Rootfs,
    Vmstate,
    Metadata,
}

impl ArtifactRole {
    fn file_name(self) -> &'static str {
        match self {
            Self::Memory => "memory.bin",
            Self::Rootfs => "rootfs.bin",
            Self::Vmstate => "vmstate.bin",
            Self::Metadata => "metadata.bin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmArtifactChunk {
    pub ordinal: u64,
    pub offset: u64,
    pub content_ref: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmArtifact {
    pub role: ArtifactRole,
    pub logical_size: u64,
    pub chunks: Vec<VmArtifactChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBackendContract {
    pub backend_id: String,
    pub host_os: String,
    pub required_features: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuContract {
    pub vcpu_count: u32,
    pub required_features: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceContract {
    pub required_features: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkContract {
    pub required_features: BTreeSet<String>,
    pub tap_device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VsockContract {
    pub required_features: BTreeSet<String>,
    pub uds_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryContract {
    pub guest_memory_mib: u64,
    pub minimum_host_memory_mib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProvenance {
    pub captured_at: String,
    pub backend_implementation_id: String,
    pub source_realization_id: String,
    pub capture_barrier_complete: bool,
    pub realization_quiesced: bool,
    #[serde(default)]
    pub placement_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSnapshotDescriptor {
    pub version: u32,
    pub target_computation_ref: String,
    pub record_frontier_ref: Option<String>,
    pub backend: String,
    pub snapshot_format: String,
    pub architecture: String,
    pub guest_os: String,
    pub host_backend_contract: HostBackendContract,
    pub cpu_contract: CpuContract,
    pub firecracker_version: String,
    pub device_contract: DeviceContract,
    pub network_contract: NetworkContract,
    pub vsock_contract: VsockContract,
    pub memory_contract: MemoryContract,
    pub artifacts: Vec<VmArtifact>,
    pub state_contract_refs: Vec<String>,
    pub contracts: Vec<ContractDescriptor>,
    pub capture_provenance: CaptureProvenance,
}

#[derive(Debug, Clone)]
pub struct VmCaptureRequest {
    pub target: ComputationRef,
}

#[derive(Debug, Clone)]
pub struct CapturedArtifact {
    pub role: ArtifactRole,
    pub path: PathBuf,
}

pub trait CaptureGuard: Send {
    fn cleanup(&mut self) -> Result<(), VmSnapshotError>;
}

pub struct CapturedVm {
    pub target: ComputationRef,
    pub record_frontier_ref: ContentRef,
    pub snapshot_format: String,
    pub architecture: String,
    pub guest_os: String,
    pub host_backend_contract: HostBackendContract,
    pub cpu_contract: CpuContract,
    pub firecracker_version: String,
    pub device_contract: DeviceContract,
    pub network_contract: NetworkContract,
    pub vsock_contract: VsockContract,
    pub memory_contract: MemoryContract,
    pub artifacts: Vec<CapturedArtifact>,
    pub state_contract_refs: Vec<ContentRef>,
    pub provenance: CaptureProvenance,
    pub guard: Box<dyn CaptureGuard>,
}

impl Drop for CapturedVm {
    fn drop(&mut self) {
        let _ = self.guard.cleanup();
    }
}

pub struct VmRestoreRequest<'a> {
    pub descriptor: &'a VmSnapshotDescriptor,
    pub artifacts: &'a BTreeMap<ArtifactRole, PathBuf>,
}

pub trait VmBackendSession: Send {
    fn activate(&mut self) -> Result<(), VmSnapshotError>;
    fn publish(&mut self) -> Result<(), VmSnapshotError>;
    fn wait(&mut self) -> Result<(), VmSnapshotError>;
    fn quiesce(&mut self) -> Result<(), VmSnapshotError>;
}

/// Backend implementations own process, TAP, namespace, vsock, temporary
/// device, and slot cleanup. They must clean partial allocations before an
/// error is returned.
pub trait VmSnapshotBackend: Send + Sync {
    fn id(&self) -> &str;
    fn capture(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError>;
    fn restore(
        &self,
        request: &VmRestoreRequest<'_>,
    ) -> Result<Box<dyn VmBackendSession>, VmSnapshotError>;
}

/// Product bridge that confirms a reference was sealed by the Record Writer.
/// The Materializer depends on this capability, not on writer storage internals.
pub trait SealedRecordFrontierVerifier: Send + Sync {
    fn verify(
        &self,
        reference: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<(), VmSnapshotError>;
}

pub struct VmSnapshotMaterializer {
    backend: Arc<dyn VmSnapshotBackend>,
    frontiers: Arc<dyn SealedRecordFrontierVerifier>,
}

impl VmSnapshotMaterializer {
    pub fn new(
        backend: Arc<dyn VmSnapshotBackend>,
        frontiers: Arc<dyn SealedRecordFrontierVerifier>,
    ) -> Self {
        Self { backend, frontiers }
    }

    fn load_verified_descriptor(
        &self,
        reference: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<VmSnapshotDescriptor, VmSnapshotError> {
        let descriptor = load_descriptor(reference, objects)?;
        let frontier = ContentRef::parse(
            descriptor
                .record_frontier_ref
                .as_ref()
                .expect("descriptor validation requires a RecordFrontier"),
        )
        .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
        self.frontiers.verify(&frontier, objects)?;
        Ok(descriptor)
    }
}

impl Materializer for VmSnapshotMaterializer {
    fn id(&self) -> &str {
        VM_SNAPSHOT_MATERIALIZER_ID
    }

    fn path_kind(&self) -> MaterializationPathKind {
        MaterializationPathKind::VmSnapshot
    }

    fn restore_capability(&self) -> RestoreCapability {
        RestoreCapability::Supported
    }

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError> {
        resolve_computation(context.objects, target)?;
        let captured = self
            .backend
            .capture(&VmCaptureRequest {
                target: target.clone(),
            })
            .map_err(materializer_operation)?;
        validate_capture_result(&captured, target, self.backend.id())
            .map_err(materializer_operation)?;
        self.frontiers
            .verify(&captured.record_frontier_ref, context.objects)
            .map_err(materializer_operation)?;
        let mut artifacts = Vec::with_capacity(captured.artifacts.len());
        for artifact in &captured.artifacts {
            artifacts
                .push(chunk_artifact(artifact, context.objects).map_err(materializer_operation)?);
        }
        let descriptor = VmSnapshotDescriptor {
            version: VM_SNAPSHOT_DESCRIPTOR_VERSION,
            target_computation_ref: target.to_string(),
            record_frontier_ref: Some(captured.record_frontier_ref.to_string()),
            backend: self.backend.id().to_owned(),
            snapshot_format: captured.snapshot_format.clone(),
            architecture: captured.architecture.clone(),
            guest_os: captured.guest_os.clone(),
            host_backend_contract: captured.host_backend_contract.clone(),
            cpu_contract: captured.cpu_contract.clone(),
            firecracker_version: captured.firecracker_version.clone(),
            device_contract: captured.device_contract.clone(),
            network_contract: captured.network_contract.clone(),
            vsock_contract: captured.vsock_contract.clone(),
            memory_contract: captured.memory_contract.clone(),
            artifacts,
            state_contract_refs: captured
                .state_contract_refs
                .iter()
                .map(ToString::to_string)
                .collect(),
            contracts: context.contracts.to_vec(),
            capture_provenance: captured.provenance.clone(),
        };
        validate_descriptor(&descriptor, context.objects).map_err(materializer_operation)?;
        Ok(context.objects.put(&serde_jcs::to_vec(&descriptor)?)?)
    }

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
        let descriptor = self
            .load_verified_descriptor(descriptor, context.objects)
            .map_err(materializer_operation)?;
        ComputationRef::parse(descriptor.target_computation_ref)
            .map_err(|error| MaterializerError::Operation(error.to_string()))
    }

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility {
        let Ok(descriptor) = self.load_verified_descriptor(descriptor, context.objects) else {
            return Compatibility::Incompatible;
        };
        let Some(capabilities) = context.runner_capabilities else {
            return Compatibility::Unknown;
        };
        if compatible(&descriptor, capabilities) {
            Compatibility::Compatible
        } else {
            Compatibility::Incompatible
        }
    }

    fn contracts(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Vec<ContractDescriptor>, MaterializerError> {
        Ok(self
            .load_verified_descriptor(descriptor, context.objects)
            .map_err(materializer_operation)?
            .contracts)
    }

    fn restore(
        &self,
        descriptor_ref: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        let descriptor = self
            .load_verified_descriptor(descriptor_ref, context.objects)
            .map_err(materializer_operation)?;
        let capabilities = context.runner_capabilities.ok_or_else(|| {
            MaterializerError::Operation("VM restore requires runner capabilities".to_owned())
        })?;
        if !compatible(&descriptor, capabilities) {
            return Err(MaterializerError::Operation(
                "VM snapshot is incompatible with runner capabilities".to_owned(),
            ));
        }
        if descriptor.backend != self.backend.id() {
            return Err(MaterializerError::Operation(format!(
                "backend `{}` cannot restore descriptor backend `{}`",
                self.backend.id(),
                descriptor.backend
            )));
        }
        let runtime_root = context.workspace.join(".capsule").join("vm-runtime");
        fs::create_dir_all(&runtime_root).map_err(materializer_operation)?;
        let temporary = tempfile::Builder::new()
            .prefix("restore-")
            .tempdir_in(runtime_root)
            .map_err(materializer_operation)?;
        let artifacts = materialize_artifacts(&descriptor, context.objects, temporary.path())
            .map_err(materializer_operation)?;
        let session = self
            .backend
            .restore(&VmRestoreRequest {
                descriptor: &descriptor,
                artifacts: &artifacts,
            })
            .map_err(materializer_operation)?;
        let target = ComputationRef::parse(&descriptor.target_computation_ref)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        Ok(Box::new(VmRealization {
            target,
            session,
            temporary: Some(temporary),
            cleaned: false,
        }))
    }
}

struct VmRealization {
    target: ComputationRef,
    session: Box<dyn VmBackendSession>,
    temporary: Option<TempDir>,
    cleaned: bool,
}

impl Realization for VmRealization {
    fn target(&self) -> &ComputationRef {
        &self.target
    }
    fn activate(&mut self) -> Result<(), MaterializerError> {
        self.session.activate().map_err(materializer_operation)
    }
    fn publish(&mut self) -> Result<(), MaterializerError> {
        self.session.publish().map_err(materializer_operation)
    }
    fn wait(&mut self) -> Result<(), MaterializerError> {
        self.session.wait().map_err(materializer_operation)
    }
    fn quiesce(&mut self) -> Result<(), MaterializerError> {
        let result = self.session.quiesce().map_err(materializer_operation);
        self.cleaned = result.is_ok();
        if self.cleaned {
            self.temporary.take();
        }
        result
    }
}

impl Drop for VmRealization {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.session.quiesce();
        }
        self.temporary.take();
    }
}

fn validate_capture_result(
    captured: &CapturedVm,
    target: &ComputationRef,
    backend_id: &str,
) -> Result<(), VmSnapshotError> {
    if &captured.target != target {
        return Err(VmSnapshotError::InvalidDescriptor(
            "capture returned a different target ComputationRef".to_owned(),
        ));
    }
    if captured.host_backend_contract.backend_id != backend_id
        || !captured.provenance.capture_barrier_complete
        || !captured.provenance.realization_quiesced
        || captured.provenance.source_realization_id.is_empty()
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "capture did not prove a barrier-synchronized quiesced active Realization".to_owned(),
        ));
    }
    if backend_id == "firecracker"
        && !captured
            .artifacts
            .iter()
            .any(|artifact| artifact.role == ArtifactRole::Metadata)
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "Firecracker capture omitted its backend restore layout metadata".to_owned(),
        ));
    }
    Ok(())
}

fn chunk_artifact(
    artifact: &CapturedArtifact,
    objects: &dyn ato_objects::ObjectStore,
) -> Result<VmArtifact, VmSnapshotError> {
    let mut file = File::open(&artifact.path)?;
    let logical_size = file.metadata()?.len();
    if logical_size == 0 {
        return Err(VmSnapshotError::InvalidDescriptor(
            "VM artifacts must not be empty".to_owned(),
        ));
    }
    let mut chunks = Vec::new();
    let mut offset = 0_u64;
    loop {
        let mut bytes = vec![0_u8; MAX_CHUNK_BYTES];
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        bytes.truncate(count);
        let content_ref = objects.put(&bytes)?;
        chunks.push(VmArtifactChunk {
            ordinal: chunks.len() as u64,
            offset,
            content_ref: content_ref.to_string(),
            size: count as u64,
        });
        offset += count as u64;
    }
    Ok(VmArtifact {
        role: artifact.role,
        logical_size,
        chunks,
    })
}

fn load_descriptor(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<VmSnapshotDescriptor, VmSnapshotError> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(objects, reference, metadata.size, MAX_DESCRIPTOR_BYTES)?;
    let descriptor: VmSnapshotDescriptor = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&descriptor)? != bytes {
        return Err(VmSnapshotError::InvalidDescriptor(
            "VM snapshot descriptor is not canonical JCS".to_owned(),
        ));
    }
    validate_descriptor(&descriptor, objects)?;
    Ok(descriptor)
}

fn validate_descriptor(
    descriptor: &VmSnapshotDescriptor,
    objects: &dyn ObjectResolver,
) -> Result<(), VmSnapshotError> {
    if descriptor.version != VM_SNAPSHOT_DESCRIPTOR_VERSION {
        return Err(VmSnapshotError::InvalidDescriptor(format!(
            "unsupported VM descriptor version {}",
            descriptor.version
        )));
    }
    let target = ComputationRef::parse(&descriptor.target_computation_ref)
        .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
    resolve_computation(objects, &target)?;
    let frontier_ref = descriptor.record_frontier_ref.as_ref().ok_or_else(|| {
        VmSnapshotError::InvalidDescriptor(
            "new VM snapshots require RecordFrontier provenance".to_owned(),
        )
    })?;
    let frontier_ref = ContentRef::parse(frontier_ref)
        .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
    let frontier_metadata = objects.metadata(&frontier_ref)?;
    read_exact_object(
        objects,
        &frontier_ref,
        frontier_metadata.size,
        MAX_FRONTIER_BYTES,
    )?;
    if descriptor.backend.is_empty()
        || descriptor.snapshot_format.is_empty()
        || descriptor.architecture.is_empty()
        || descriptor.guest_os.is_empty()
        || descriptor.host_backend_contract.backend_id != descriptor.backend
        || descriptor.host_backend_contract.host_os.is_empty()
        || descriptor.cpu_contract.vcpu_count == 0
        || descriptor.memory_contract.guest_memory_mib == 0
        || descriptor.memory_contract.minimum_host_memory_mib
            < descriptor.memory_contract.guest_memory_mib
        || Version::parse(&descriptor.firecracker_version).is_err()
        || !descriptor.capture_provenance.capture_barrier_complete
        || !descriptor.capture_provenance.realization_quiesced
        || descriptor
            .capture_provenance
            .source_realization_id
            .is_empty()
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "VM snapshot compatibility or capture fields are incomplete".to_owned(),
        ));
    }
    if descriptor
        .network_contract
        .required_features
        .contains("tap")
        && !descriptor
            .network_contract
            .tap_device
            .as_deref()
            .is_some_and(valid_interface_name)
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "tap network contract requires a valid host interface name".to_owned(),
        ));
    }
    if descriptor
        .vsock_contract
        .required_features
        .contains("vsock-uds")
        && !descriptor
            .vsock_contract
            .uds_path
            .as_deref()
            .is_some_and(valid_relative_path)
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "vsock contract requires a normalized backend-relative UDS path".to_owned(),
        ));
    }
    if descriptor.artifacts.is_empty()
        || descriptor.artifacts.len() > MAX_ARTIFACTS
        || descriptor
            .artifacts
            .iter()
            .map(|artifact| artifact.chunks.len())
            .sum::<usize>()
            > MAX_CHUNKS
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "VM snapshot artifact closure exceeds bounds".to_owned(),
        ));
    }
    let mut roles = BTreeSet::new();
    for artifact in &descriptor.artifacts {
        if !roles.insert(artifact.role) || artifact.logical_size == 0 {
            return Err(VmSnapshotError::InvalidDescriptor(
                "duplicate or empty VM artifact".to_owned(),
            ));
        }
        let mut expected_offset = 0_u64;
        for (ordinal, chunk) in artifact.chunks.iter().enumerate() {
            if chunk.ordinal != ordinal as u64
                || chunk.offset != expected_offset
                || chunk.size == 0
                || chunk.size > MAX_CHUNK_BYTES as u64
            {
                return Err(VmSnapshotError::InvalidDescriptor(
                    "VM chunks must be contiguous, ordered, non-overlapping, and bounded"
                        .to_owned(),
                ));
            }
            let reference = ContentRef::parse(&chunk.content_ref)
                .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
            read_exact_object(objects, &reference, chunk.size, MAX_CHUNK_BYTES as u64)?;
            expected_offset = expected_offset.checked_add(chunk.size).ok_or_else(|| {
                VmSnapshotError::InvalidDescriptor("chunk size overflow".to_owned())
            })?;
        }
        if artifact.chunks.is_empty() || expected_offset != artifact.logical_size {
            return Err(VmSnapshotError::InvalidDescriptor(
                "VM artifact logical size does not match its chunk closure".to_owned(),
            ));
        }
    }
    if ![
        ArtifactRole::Memory,
        ArtifactRole::Rootfs,
        ArtifactRole::Vmstate,
    ]
    .iter()
    .all(|role| roles.contains(role))
    {
        return Err(VmSnapshotError::InvalidDescriptor(
            "VM snapshot requires memory, rootfs, and vmstate artifacts".to_owned(),
        ));
    }
    if descriptor.backend == "firecracker" && !roles.contains(&ArtifactRole::Metadata) {
        return Err(VmSnapshotError::InvalidDescriptor(
            "Firecracker snapshot requires restore layout metadata".to_owned(),
        ));
    }
    if descriptor.backend == "firecracker" {
        let metadata = descriptor
            .artifacts
            .iter()
            .find(|artifact| artifact.role == ArtifactRole::Metadata)
            .expect("metadata role was checked");
        if metadata.logical_size > 1024 * 1024 {
            return Err(VmSnapshotError::InvalidDescriptor(
                "Firecracker restore layout metadata exceeds 1 MiB".to_owned(),
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.logical_size as usize);
        for chunk in &metadata.chunks {
            let reference = ContentRef::parse(&chunk.content_ref)
                .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
            bytes.extend(read_exact_object(
                objects,
                &reference,
                chunk.size,
                MAX_CHUNK_BYTES as u64,
            )?);
        }
        let layout = FirecrackerRestoreLayout::decode(&bytes)?;
        if layout.vsock_uds_path.as_deref() != descriptor.vsock_contract.uds_path.as_deref() {
            return Err(VmSnapshotError::InvalidDescriptor(
                "Firecracker restore layout vsock path disagrees with descriptor contract"
                    .to_owned(),
            ));
        }
    }
    let mut states = BTreeSet::new();
    for reference in &descriptor.state_contract_refs {
        if !states.insert(reference) {
            return Err(VmSnapshotError::InvalidDescriptor(
                "duplicate state contract reference".to_owned(),
            ));
        }
        let reference = ContentRef::parse(reference)
            .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
        objects.metadata(&reference)?;
    }
    for contract in &descriptor.contracts {
        contract
            .validate()
            .map_err(|error| VmSnapshotError::InvalidDescriptor(error.to_string()))?;
    }
    Ok(())
}

fn valid_interface_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 15
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn compatible(descriptor: &VmSnapshotDescriptor, runner: &RunnerCapabilities) -> bool {
    runner.architecture == descriptor.architecture
        && runner.host_os == descriptor.host_backend_contract.host_os
        && runner.backends.contains(&descriptor.backend)
        && runner.guest_os.contains(&descriptor.guest_os)
        && runner
            .snapshot_formats
            .contains(&descriptor.snapshot_format)
        && runner
            .backend_versions
            .get(&descriptor.backend)
            .is_some_and(|version| version == &descriptor.firecracker_version)
        && descriptor
            .host_backend_contract
            .required_features
            .is_subset(&runner.device_features)
        && descriptor
            .cpu_contract
            .required_features
            .is_subset(&runner.cpu_features)
        && runner.memory_mib >= descriptor.memory_contract.minimum_host_memory_mib
        && descriptor
            .device_contract
            .required_features
            .is_subset(&runner.device_features)
        && descriptor
            .network_contract
            .required_features
            .is_subset(&runner.network_features)
        && descriptor
            .vsock_contract
            .required_features
            .is_subset(&runner.vsock_features)
}

fn materialize_artifacts(
    descriptor: &VmSnapshotDescriptor,
    objects: &dyn ObjectResolver,
    root: &Path,
) -> Result<BTreeMap<ArtifactRole, PathBuf>, VmSnapshotError> {
    let mut paths = BTreeMap::new();
    for artifact in &descriptor.artifacts {
        let path = root.join(artifact.role.file_name());
        let mut output = File::create(&path)?;
        for chunk in &artifact.chunks {
            let reference = ContentRef::parse(&chunk.content_ref)
                .map_err(|error| VmSnapshotError::InvalidReference(error.to_string()))?;
            let bytes = read_exact_object(objects, &reference, chunk.size, MAX_CHUNK_BYTES as u64)?;
            output.write_all(&bytes)?;
        }
        output.sync_all()?;
        if output.metadata()?.len() != artifact.logical_size {
            return Err(VmSnapshotError::InvalidDescriptor(
                "materialized VM artifact length mismatch".to_owned(),
            ));
        }
        paths.insert(artifact.role, path);
    }
    Ok(paths)
}

#[derive(Default)]
pub struct VmSnapshotReferences;

impl MaterializationReferences for VmSnapshotReferences {
    fn materializer_id(&self) -> &str {
        VM_SNAPSHOT_MATERIALIZER_ID
    }

    fn outgoing(
        &self,
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let descriptor = load_descriptor(descriptor, objects).map_err(bundle_error)?;
        let mut links = vec![ObjectLink::Computation(
            ComputationRef::parse(descriptor.target_computation_ref).map_err(|error| {
                BundleError::InvalidReference {
                    value: "VM target_computation_ref".to_owned(),
                    reason: error.to_string(),
                }
            })?,
        )];
        if let Some(frontier) = descriptor.record_frontier_ref {
            links.push(ObjectLink::Content(ContentRef::parse(frontier).map_err(
                |error| BundleError::InvalidReference {
                    value: "VM record_frontier_ref".to_owned(),
                    reason: error.to_string(),
                },
            )?));
        }
        for artifact in descriptor.artifacts {
            for chunk in artifact.chunks {
                links.push(ObjectLink::Content(
                    ContentRef::parse(chunk.content_ref).map_err(|error| {
                        BundleError::InvalidReference {
                            value: "VM artifact chunk".to_owned(),
                            reason: error.to_string(),
                        }
                    })?,
                ));
            }
        }
        for state in descriptor.state_contract_refs {
            links.push(ObjectLink::Content(ContentRef::parse(state).map_err(
                |error| BundleError::InvalidReference {
                    value: "VM state contract".to_owned(),
                    reason: error.to_string(),
                },
            )?));
        }
        Ok(links)
    }
}

fn bundle_error(error: VmSnapshotError) -> BundleError {
    BundleError::InvalidReference {
        value: "VM snapshot descriptor".to_owned(),
        reason: error.to_string(),
    }
}

fn materializer_operation(error: impl std::fmt::Display) -> MaterializerError {
    MaterializerError::Operation(error.to_string())
}

#[derive(Debug, Error)]
pub enum VmSnapshotError {
    #[error("invalid VM snapshot descriptor: {0}")]
    InvalidDescriptor(String),
    #[error("invalid VM snapshot reference: {0}")]
    InvalidReference(String),
    #[error("VM backend failure: {0}")]
    Backend(String),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use ato_adapter_api::{AdapterRegistry, Stylus, SupportedOperation, WorkspaceCapturePolicy};
    use ato_computation::{
        ComputationObject, OperationId, PortId, ProtocolId, SemanticsId, computation_ref,
        encode_computation_object,
    };
    use ato_materializer_api::{
        ContractContext, ContractResult, ContractVerifier, ContractVerifierRegistry,
        accept_candidate,
    };
    use ato_objects::{MemoryObjectStore, ObjectMetadata, ObjectStore, RecordCandidate};
    use ato_record_writer::{RecordPipeline, RecordSchemaRegistry, RecordWriterConfig};

    use super::*;

    static RUN: AtomicU64 = AtomicU64::new(1);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SessionFailure {
        None,
        Restore,
        Activate,
        Publish,
        Wait,
        Quiesce,
    }

    struct Guard {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl CaptureGuard for Guard {
        fn cleanup(&mut self) -> Result<(), VmSnapshotError> {
            self.events
                .lock()
                .unwrap()
                .push("capture.cleanup".to_owned());
            Ok(())
        }
    }

    struct FakeBackend {
        root: PathBuf,
        frontier: Arc<Mutex<ContentRef>>,
        events: Arc<Mutex<Vec<String>>>,
        active_resources: Arc<AtomicUsize>,
        failure: Arc<Mutex<SessionFailure>>,
    }

    impl FakeBackend {
        fn set_failure(&self, failure: SessionFailure) {
            *self.failure.lock().unwrap() = failure;
        }
    }

    impl VmSnapshotBackend for FakeBackend {
        fn id(&self) -> &str {
            "firecracker"
        }

        fn capture(&self, request: &VmCaptureRequest) -> Result<CapturedVm, VmSnapshotError> {
            self.events.lock().unwrap().push("capture".to_owned());
            Ok(CapturedVm {
                target: request.target.clone(),
                record_frontier_ref: self.frontier.lock().unwrap().clone(),
                snapshot_format: "fc-full-file-v1".to_owned(),
                architecture: "x86_64".to_owned(),
                guest_os: "linux".to_owned(),
                host_backend_contract: HostBackendContract {
                    backend_id: "firecracker".to_owned(),
                    host_os: "linux".to_owned(),
                    required_features: BTreeSet::from(["kvm".to_owned()]),
                },
                cpu_contract: CpuContract {
                    vcpu_count: 2,
                    required_features: BTreeSet::from(["sse4.2".to_owned()]),
                },
                firecracker_version: "1.7.0".to_owned(),
                device_contract: DeviceContract {
                    required_features: BTreeSet::from(["virtio-blk".to_owned()]),
                },
                network_contract: NetworkContract {
                    required_features: BTreeSet::from(["tap".to_owned()]),
                    tap_device: Some("ato-test-tap0".to_owned()),
                },
                vsock_contract: VsockContract {
                    required_features: BTreeSet::from(["vsock-uds".to_owned()]),
                    uds_path: Some("vsock/guest.sock".to_owned()),
                },
                memory_contract: MemoryContract {
                    guest_memory_mib: 512,
                    minimum_host_memory_mib: 1024,
                },
                artifacts: vec![
                    CapturedArtifact {
                        role: ArtifactRole::Memory,
                        path: self.root.join("memory.bin"),
                    },
                    CapturedArtifact {
                        role: ArtifactRole::Rootfs,
                        path: self.root.join("rootfs.bin"),
                    },
                    CapturedArtifact {
                        role: ArtifactRole::Vmstate,
                        path: self.root.join("vmstate.bin"),
                    },
                    CapturedArtifact {
                        role: ArtifactRole::Metadata,
                        path: self.root.join("restore-layout.json"),
                    },
                ],
                state_contract_refs: Vec::new(),
                provenance: CaptureProvenance {
                    captured_at: "2030-01-01T00:00:00Z".to_owned(),
                    backend_implementation_id: "fake.firecracker@1".to_owned(),
                    source_realization_id: "realization.active-1".to_owned(),
                    capture_barrier_complete: true,
                    realization_quiesced: true,
                    placement_hint: Some("staging-linux".to_owned()),
                },
                guard: Box::new(Guard {
                    events: Arc::clone(&self.events),
                }),
            })
        }

        fn restore(
            &self,
            request: &VmRestoreRequest<'_>,
        ) -> Result<Box<dyn VmBackendSession>, VmSnapshotError> {
            self.events.lock().unwrap().extend([
                "slot.allocate".to_owned(),
                "tap.create".to_owned(),
                "vsock.create".to_owned(),
                "process.spawn".to_owned(),
            ]);
            for role in [
                ArtifactRole::Memory,
                ArtifactRole::Rootfs,
                ArtifactRole::Vmstate,
            ] {
                if !request
                    .artifacts
                    .get(&role)
                    .is_some_and(|path| path.is_file())
                {
                    return Err(VmSnapshotError::Backend(
                        "missing restored artifact".to_owned(),
                    ));
                }
            }
            if *self.failure.lock().unwrap() == SessionFailure::Restore {
                self.events.lock().unwrap().extend([
                    "process.cleanup".to_owned(),
                    "vsock.cleanup".to_owned(),
                    "tap.cleanup".to_owned(),
                    "slot.cleanup".to_owned(),
                ]);
                return Err(VmSnapshotError::Backend("restore failure".to_owned()));
            }
            self.active_resources.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeSession {
                events: Arc::clone(&self.events),
                active_resources: Arc::clone(&self.active_resources),
                failure: *self.failure.lock().unwrap(),
                cleaned: false,
            }))
        }
    }

    struct FakeSession {
        events: Arc<Mutex<Vec<String>>>,
        active_resources: Arc<AtomicUsize>,
        failure: SessionFailure,
        cleaned: bool,
    }

    impl FakeSession {
        fn step(&self, name: &str, failure: SessionFailure) -> Result<(), VmSnapshotError> {
            self.events.lock().unwrap().push(name.to_owned());
            if self.failure == failure {
                Err(VmSnapshotError::Backend(format!("{name} failure")))
            } else {
                Ok(())
            }
        }
    }

    impl VmBackendSession for FakeSession {
        fn activate(&mut self) -> Result<(), VmSnapshotError> {
            self.step("activate", SessionFailure::Activate)
        }
        fn publish(&mut self) -> Result<(), VmSnapshotError> {
            self.step("publish", SessionFailure::Publish)
        }
        fn wait(&mut self) -> Result<(), VmSnapshotError> {
            self.step("wait", SessionFailure::Wait)
        }
        fn quiesce(&mut self) -> Result<(), VmSnapshotError> {
            if self.cleaned {
                return Ok(());
            }
            self.events.lock().unwrap().extend([
                "process.cleanup".to_owned(),
                "vsock.cleanup".to_owned(),
                "tap.cleanup".to_owned(),
                "slot.cleanup".to_owned(),
            ]);
            self.active_resources.fetch_sub(1, Ordering::SeqCst);
            self.cleaned = true;
            if self.failure == SessionFailure::Quiesce {
                Err(VmSnapshotError::Backend("quiesce failure".to_owned()))
            } else {
                Ok(())
            }
        }
    }

    impl Drop for FakeSession {
        fn drop(&mut self) {
            let _ = self.quiesce();
        }
    }

    struct Harness {
        temporary: TempDir,
        objects: Arc<MemoryObjectStore>,
        target: ComputationRef,
        frontier: ContentRef,
        capture_frontier: Arc<Mutex<ContentRef>>,
        adapters: AdapterRegistry,
        policy: WorkspaceCapturePolicy,
        capabilities: RunnerCapabilities,
        backend: Arc<FakeBackend>,
        materializer: VmSnapshotMaterializer,
    }

    struct WriterFrontierVerifier;

    impl SealedRecordFrontierVerifier for WriterFrontierVerifier {
        fn verify(
            &self,
            reference: &ContentRef,
            objects: &dyn ObjectResolver,
        ) -> Result<(), VmSnapshotError> {
            ato_record_writer::verify_frontier_object(reference, objects)
                .map(|_| ())
                .map_err(|error| VmSnapshotError::InvalidDescriptor(error.to_string()))
        }
    }

    impl Harness {
        fn new() -> Self {
            let temporary = task_tempdir("vm-materializer");
            for (name, bytes) in [
                ("memory.bin", b"memory-state".as_slice()),
                ("rootfs.bin", b"root-filesystem".as_slice()),
                ("vmstate.bin", b"device-state".as_slice()),
                ("state.contract", b"state-contract".as_slice()),
            ] {
                fs::write(temporary.path().join(name), bytes).unwrap();
            }
            fs::write(
                temporary.path().join("restore-layout.json"),
                FirecrackerRestoreLayout::default().encode().unwrap(),
            )
            .unwrap();
            let objects = Arc::new(MemoryObjectStore::default());
            let target = computation(objects.as_ref());
            let frontier = frontier(Arc::clone(&objects), temporary.path());
            let events = Arc::new(Mutex::new(Vec::new()));
            let capture_frontier = Arc::new(Mutex::new(frontier.clone()));
            let backend = Arc::new(FakeBackend {
                root: temporary.path().to_path_buf(),
                frontier: Arc::clone(&capture_frontier),
                events,
                active_resources: Arc::new(AtomicUsize::new(0)),
                failure: Arc::new(Mutex::new(SessionFailure::None)),
            });
            let backend_trait: Arc<dyn VmSnapshotBackend> = backend.clone();
            Self {
                temporary,
                objects,
                target,
                frontier,
                capture_frontier,
                adapters: AdapterRegistry::default(),
                policy: WorkspaceCapturePolicy::secure_default(),
                capabilities: capabilities(),
                backend,
                materializer: VmSnapshotMaterializer::new(
                    backend_trait,
                    Arc::new(WriterFrontierVerifier),
                ),
            }
        }

        fn context(&self) -> MaterializerContext<'_> {
            MaterializerContext {
                objects: self.objects.as_ref(),
                adapters: &self.adapters,
                records: &[],
                records_v2: &[],
                replay_anchor: None,
                record_frontier_ref: None,
                workspace: self.temporary.path(),
                workspace_policy: &self.policy,
                realization: None,
                contracts: &[],
                runner_capabilities: Some(&self.capabilities),
            }
        }

        fn encode(&self) -> ContentRef {
            self.materializer
                .encode(&self.target, &self.context())
                .unwrap()
        }

        fn descriptor(&self, reference: &ContentRef) -> VmSnapshotDescriptor {
            let metadata = self.objects.metadata(reference).unwrap();
            let bytes = read_exact_object(
                self.objects.as_ref(),
                reference,
                metadata.size,
                MAX_DESCRIPTOR_BYTES,
            )
            .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        fn store_descriptor(&self, descriptor: &VmSnapshotDescriptor) -> ContentRef {
            self.objects
                .put(&serde_jcs::to_vec(descriptor).unwrap())
                .unwrap()
        }
    }

    fn task_tempdir(prefix: &str) -> TempDir {
        let root = std::env::current_dir().unwrap().join(".tmp");
        fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(root)
            .unwrap()
    }

    fn computation(objects: &MemoryObjectStore) -> ComputationRef {
        let residual = objects.put(b"known residual").unwrap();
        let object = ComputationObject {
            semantics: SemanticsId::parse("example.vm-target@1").unwrap(),
            boundary: BTreeMap::new(),
            residual,
        };
        let target = computation_ref(&object).unwrap();
        objects
            .insert(
                target.content_ref(),
                &encode_computation_object(&object).unwrap(),
            )
            .unwrap();
        target
    }

    fn frontier(objects: Arc<MemoryObjectStore>, root: &Path) -> ContentRef {
        let mut schemas = RecordSchemaRegistry::default();
        schemas
            .register(
                SupportedOperation::new("ato.pty@1", "input", 1, BTreeSet::new()).unwrap(),
                |_| Ok(()),
            )
            .unwrap();
        let records_root = root.join(format!("records-{}", RUN.load(Ordering::SeqCst)));
        let run = format!("run-{}", RUN.fetch_add(1, Ordering::SeqCst));
        let object_store: Arc<dyn ObjectStore> = objects.clone();
        let pipeline = RecordPipeline::start(
            RecordWriterConfig::at(records_root, run),
            object_store,
            schemas,
        )
        .unwrap();
        pipeline
            .stylus
            .record(RecordCandidate {
                protocol_id: ProtocolId::parse("ato.pty@1").unwrap(),
                operation_id: OperationId::parse("input").unwrap(),
                port_id: PortId::parse("terminal.main").unwrap(),
                payload: b"x".to_vec(),
                payload_version: 1,
                required_features: BTreeSet::new(),
                recorded_by: Some("test.stylus@1".to_owned()),
                stream: "pty.main".to_owned(),
                local_seq: 1,
                caused_by: Vec::new(),
                observed_at: "2030-01-01T00:00:00Z".to_owned(),
            })
            .unwrap();
        pipeline.barrier.seal().unwrap().frontier_digest
    }

    fn capabilities() -> RunnerCapabilities {
        RunnerCapabilities {
            architecture: "x86_64".to_owned(),
            host_os: "linux".to_owned(),
            backends: BTreeSet::from(["firecracker".to_owned()]),
            backend_versions: BTreeMap::from([("firecracker".to_owned(), "1.7.0".to_owned())]),
            guest_os: BTreeSet::from(["linux".to_owned()]),
            snapshot_formats: BTreeSet::from(["fc-full-file-v1".to_owned()]),
            cpu_features: BTreeSet::from(["sse4.2".to_owned()]),
            memory_mib: 4096,
            device_features: BTreeSet::from(["kvm".to_owned(), "virtio-blk".to_owned()]),
            network_features: BTreeSet::from(["tap".to_owned()]),
            vsock_features: BTreeSet::from(["vsock-uds".to_owned()]),
        }
    }

    #[test]
    fn canonical_descriptor_roundtrip_targets_existing_computation_and_frontier() {
        let harness = Harness::new();
        let reference = harness.encode();
        let descriptor = load_descriptor(&reference, harness.objects.as_ref()).unwrap();

        assert_eq!(
            descriptor.target_computation_ref,
            harness.target.to_string()
        );
        assert_eq!(
            descriptor.record_frontier_ref,
            Some(harness.frontier.to_string())
        );
        assert_eq!(
            harness
                .materializer
                .verify(&reference, &harness.context())
                .unwrap(),
            harness.target
        );
        assert_ne!(reference, *harness.target.content_ref());
    }

    #[test]
    fn physical_bytes_and_frontier_never_change_target_computation_identity() {
        let harness = Harness::new();
        let first = harness.encode();
        fs::write(
            harness.temporary.path().join("memory.bin"),
            b"different-memory",
        )
        .unwrap();
        let second = harness.encode();
        assert_ne!(first, second);
        assert_eq!(
            harness
                .materializer
                .verify(&first, &harness.context())
                .unwrap(),
            harness.target
        );
        assert_eq!(
            harness
                .materializer
                .verify(&second, &harness.context())
                .unwrap(),
            harness.target
        );

        let second_frontier = frontier(Arc::clone(&harness.objects), harness.temporary.path());
        *harness.capture_frontier.lock().unwrap() = second_frontier.clone();
        let third = harness.encode();
        assert_ne!(second, third);
        assert_eq!(
            harness
                .materializer
                .verify(&third, &harness.context())
                .unwrap(),
            harness.target
        );
    }

    #[test]
    fn missing_duplicate_overlapping_and_length_mismatched_chunks_fail_closed() {
        let harness = Harness::new();
        let valid_ref = harness.encode();
        let valid = harness.descriptor(&valid_ref);

        let mut missing = valid.clone();
        missing.artifacts[0].chunks[0].content_ref = format!("blake3:{}", "d".repeat(64));
        assert!(
            load_descriptor(
                &harness.store_descriptor(&missing),
                harness.objects.as_ref()
            )
            .is_err()
        );

        let mut duplicate = valid.clone();
        let mut chunk = duplicate.artifacts[0].chunks[0].clone();
        chunk.offset += chunk.size;
        duplicate.artifacts[0].chunks.push(chunk);
        duplicate.artifacts[0].logical_size *= 2;
        assert!(
            load_descriptor(
                &harness.store_descriptor(&duplicate),
                harness.objects.as_ref()
            )
            .is_err()
        );

        let mut overlap = valid.clone();
        overlap.artifacts[0].chunks[0].offset = 1;
        assert!(
            load_descriptor(
                &harness.store_descriptor(&overlap),
                harness.objects.as_ref()
            )
            .is_err()
        );

        let mut length = valid;
        length.artifacts[0].chunks[0].size += 1;
        length.artifacts[0].logical_size += 1;
        assert!(
            load_descriptor(&harness.store_descriptor(&length), harness.objects.as_ref()).is_err()
        );
    }

    struct CorruptResolver<'a> {
        inner: &'a MemoryObjectStore,
        corrupt: ContentRef,
    }

    impl ObjectResolver for CorruptResolver<'_> {
        fn metadata(
            &self,
            reference: &ContentRef,
        ) -> Result<ObjectMetadata, ato_objects::ObjectError> {
            self.inner.metadata(reference)
        }
        fn open(
            &self,
            reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send + '_>, ato_objects::ObjectError> {
            if reference == &self.corrupt {
                let size = self.inner.metadata(reference)?.size as usize;
                Ok(Box::new(Cursor::new(vec![b'z'; size])))
            } else {
                self.inner.open(reference)
            }
        }
    }

    #[test]
    fn digest_mismatch_fails_closed() {
        let harness = Harness::new();
        let reference = harness.encode();
        let descriptor = harness.descriptor(&reference);
        let corrupt = ContentRef::parse(&descriptor.artifacts[0].chunks[0].content_ref).unwrap();
        let resolver = CorruptResolver {
            inner: harness.objects.as_ref(),
            corrupt,
        };

        assert!(validate_descriptor(&descriptor, &resolver).is_err());
    }

    #[test]
    fn compatibility_checks_every_runner_capability_fail_closed() {
        let harness = Harness::new();
        let reference = harness.encode();
        assert_eq!(
            harness
                .materializer
                .compatibility(&reference, &harness.context()),
            Compatibility::Compatible
        );

        let mut variants = Vec::new();
        let mut architecture = harness.capabilities.clone();
        architecture.architecture = "aarch64".to_owned();
        variants.push(architecture);
        let mut guest = harness.capabilities.clone();
        guest.guest_os.clear();
        variants.push(guest);
        let mut backend = harness.capabilities.clone();
        backend.backends.clear();
        variants.push(backend);
        let mut format = harness.capabilities.clone();
        format.snapshot_formats.clear();
        variants.push(format);
        let mut version = harness.capabilities.clone();
        version
            .backend_versions
            .insert("firecracker".to_owned(), "1.6.0".to_owned());
        variants.push(version);
        let mut cpu = harness.capabilities.clone();
        cpu.cpu_features.clear();
        variants.push(cpu);
        let mut memory = harness.capabilities.clone();
        memory.memory_mib = 512;
        variants.push(memory);
        let mut device = harness.capabilities.clone();
        device.device_features.remove("virtio-blk");
        variants.push(device);
        let mut network = harness.capabilities.clone();
        network.network_features.clear();
        variants.push(network);
        let mut vsock = harness.capabilities.clone();
        vsock.vsock_features.clear();
        variants.push(vsock);

        for capabilities in variants {
            let context = MaterializerContext {
                runner_capabilities: Some(&capabilities),
                ..harness.context()
            };
            assert_eq!(
                harness.materializer.compatibility(&reference, &context),
                Compatibility::Incompatible
            );
        }
        let unknown = MaterializerContext {
            runner_capabilities: None,
            ..harness.context()
        };
        assert_eq!(
            harness.materializer.compatibility(&reference, &unknown),
            Compatibility::Unknown
        );
    }

    #[test]
    fn restore_failure_and_activation_failure_cleanup_all_owned_resources() {
        let harness = Harness::new();
        let descriptor = harness.encode();
        harness.backend.set_failure(SessionFailure::Restore);
        assert!(
            harness
                .materializer
                .restore(&descriptor, &harness.context())
                .is_err()
        );
        assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
        assert!(harness.backend.events.lock().unwrap().ends_with(&[
            "process.cleanup".to_owned(),
            "vsock.cleanup".to_owned(),
            "tap.cleanup".to_owned(),
            "slot.cleanup".to_owned(),
        ]));

        harness.backend.set_failure(SessionFailure::Activate);
        let candidate = harness
            .materializer
            .restore(&descriptor, &harness.context())
            .unwrap();
        let contract_context = ContractContext {
            objects: harness.objects.as_ref(),
            workspace: harness.temporary.path(),
        };
        assert!(
            accept_candidate(
                candidate,
                &[],
                &ContractVerifierRegistry::default(),
                &contract_context,
            )
            .is_err()
        );
        assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
    }

    struct RejectVerifier;

    impl ContractVerifier for RejectVerifier {
        fn id(&self) -> &str {
            "example.reject@1"
        }
        fn verify(
            &self,
            _: &ContractDescriptor,
            _: &mut dyn Realization,
            _: &ContractContext<'_>,
        ) -> Result<ContractResult, MaterializerError> {
            Err(MaterializerError::Operation("Contract rejected".to_owned()))
        }
    }

    #[test]
    fn contract_failure_cleans_candidate_before_publication() {
        let harness = Harness::new();
        let descriptor = harness.encode();
        let candidate = harness
            .materializer
            .restore(&descriptor, &harness.context())
            .unwrap();
        let contract = ContractDescriptor::new("example.reject@1", serde_json::json!({})).unwrap();
        let mut verifiers = ContractVerifierRegistry::default();
        verifiers.register(Arc::new(RejectVerifier)).unwrap();
        let context = ContractContext {
            objects: harness.objects.as_ref(),
            workspace: harness.temporary.path(),
        };

        assert!(accept_candidate(candidate, &[contract], &verifiers, &context).is_err());
        assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
        assert!(
            !harness
                .backend
                .events
                .lock()
                .unwrap()
                .contains(&"publish".to_owned())
        );
    }

    #[test]
    fn repeated_restore_has_no_process_tap_vsock_or_slot_leak() {
        let harness = Harness::new();
        let descriptor = harness.encode();
        for _ in 0..3 {
            let candidate = harness
                .materializer
                .restore(&descriptor, &harness.context())
                .unwrap();
            let context = ContractContext {
                objects: harness.objects.as_ref(),
                workspace: harness.temporary.path(),
            };
            let accepted = accept_candidate(
                candidate,
                &[],
                &ContractVerifierRegistry::default(),
                &context,
            )
            .unwrap();
            drop(accepted);
            assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn backend_mismatch_never_falls_back_inside_materializer() {
        let harness = Harness::new();
        let reference = harness.encode();
        let mut descriptor = harness.descriptor(&reference);
        descriptor.backend = "qemu".to_owned();
        descriptor.host_backend_contract.backend_id = "qemu".to_owned();
        let reference = harness.store_descriptor(&descriptor);

        assert!(
            harness
                .materializer
                .restore(&reference, &harness.context())
                .is_err()
        );
        assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cleanup_runs_after_publish_wait_and_quiesce_failure_points() {
        for failure in [
            SessionFailure::Publish,
            SessionFailure::Wait,
            SessionFailure::Quiesce,
        ] {
            let harness = Harness::new();
            let descriptor = harness.encode();
            harness.backend.set_failure(failure);
            let candidate = harness
                .materializer
                .restore(&descriptor, &harness.context())
                .unwrap();
            let context = ContractContext {
                objects: harness.objects.as_ref(),
                workspace: harness.temporary.path(),
            };
            if let Ok(accepted) = accept_candidate(
                candidate,
                &[],
                &ContractVerifierRegistry::default(),
                &context,
            ) {
                assert!(accepted.run().is_err());
            }
            assert_eq!(harness.backend.active_resources.load(Ordering::SeqCst), 0);
        }
    }
}
