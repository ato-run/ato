use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Result, bail};
use ato_adapter_workspace::WorkspaceSnapshot;
use ato_computation::{
    Boundary, ComputationObject, ContentRef, PortDef, PortId, ProtocolId, RoleId, SemanticsId,
    computation_ref, encode_computation_object,
};
use ato_materializer_vm_snapshot::{
    ArtifactRole, CaptureProvenance, CpuContract, DeviceContract, FirecrackerRestoreLayout,
    HostBackendContract, MemoryContract, NetworkContract, VM_SNAPSHOT_MATERIALIZER_ID, VmArtifact,
    VmArtifactChunk, VmSnapshotDescriptor, VsockContract,
};
use ato_objects::{
    GraphMaterialization, GraphObjectKind, GraphRestoreCapability, MemoryObjectStore,
    ObjectResolver, ObjectStore, export_object_graph, resolve_computation,
};

use super::*;

struct MemorySource {
    index: Vec<u8>,
    objects: BTreeMap<String, Vec<u8>>,
    corrupt: Option<String>,
}

impl RuntimeGraphSource for MemorySource {
    fn load_index(&self) -> Result<Vec<u8>> {
        Ok(self.index.clone())
    }

    fn load_object(&self, reference: &ContentRef, _expected_size: u64) -> Result<Vec<u8>> {
        let mut bytes = self
            .objects
            .get(&reference.to_string())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing fixture object"))?;
        if self.corrupt.as_deref() == Some(reference.as_str()) {
            bytes[0] ^= 1;
        }
        Ok(bytes)
    }
}

fn fixture() -> Result<(Arc<MemoryObjectStore>, ObjectGraphIndexV1)> {
    let objects = Arc::new(MemoryObjectStore::default());
    let file = objects.put(b"2048")?;
    let snapshot = objects.put(&serde_jcs::to_vec(&WorkspaceSnapshot {
        files: BTreeMap::from([("index.html".to_owned(), file.to_string())]),
    })?)?;
    let state = objects.put(&serde_jcs::to_vec(&serde_json::json!({
        "version": 1,
        "config": {
            "binding": [{"id": "display", "protocol": "ato.binding@1"}]
        },
        "workspace_snapshot": snapshot.to_string()
    }))?)?;
    let boundary = Boundary::from([(
        PortId::parse("web")?,
        PortDef {
            protocol: ProtocolId::parse("ato.http@1")?,
            role: RoleId::parse("server")?,
        },
    )]);
    let object = ComputationObject {
        semantics: SemanticsId::parse(AUTHORING_SEMANTICS_ID)?,
        boundary,
        residual: state,
    };
    let root = computation_ref(&object)?;
    objects.insert(root.content_ref(), &encode_computation_object(&object)?)?;
    let closure = export_object_graph(
        &root,
        &[],
        objects.as_ref(),
        &standard_reference_registry()?,
    )?;
    let index = ObjectGraphIndexV1::new(
        closure,
        vec![ExportedPort {
            port_id: "web".to_owned(),
            protocol: "ato.http@1".to_owned(),
            role: "server".to_owned(),
        }],
        vec![RequiredBinding {
            id: "display".to_owned(),
            schema: "ato.binding@1".to_owned(),
        }],
        VisibilityPolicy::Private,
    );
    Ok((objects, index))
}

fn source(objects: &dyn ObjectResolver, index: &ObjectGraphIndexV1) -> Result<MemorySource> {
    let mut downloaded = BTreeMap::new();
    for descriptor in &index.objects {
        let reference = ContentRef::parse(&descriptor.content_ref)?;
        let mut reader = objects.open(&reference)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut bytes)?;
        downloaded.insert(descriptor.content_ref.clone(), bytes);
    }
    Ok(MemorySource {
        index: serde_jcs::to_vec(index)?,
        objects: downloaded,
        corrupt: None,
    })
}

#[test]
fn validates_downloaded_graph_and_rederives_runtime_summary() -> Result<()> {
    let (objects, index) = fixture()?;
    let source = source(objects.as_ref(), &index)?;
    let work = tempfile::tempdir()?;
    let graph = download_and_validate_graph(
        &source,
        &GraphDownloadExpectation {
            index_digest: index.digest()?,
            root_computation_ref: index.root_computation_ref.clone(),
            object_count: index.objects.len(),
            logical_bytes: index.logical_bytes()?,
        },
        work.path(),
    )?;
    assert_eq!(graph.report().workspace_file_count, 1);
    assert_eq!(graph.report().exported_ports, index.exported_ports);
    assert_eq!(graph.report().required_bindings, index.required_bindings);
    Ok(())
}

#[test]
fn rejects_forged_declared_semantic_reference() -> Result<()> {
    let (objects, mut index) = fixture()?;
    let root = index
        .objects
        .iter_mut()
        .find(|descriptor| descriptor.content_ref == index.root_computation_ref)
        .expect("root descriptor");
    root.references.clear();
    let error =
        validate_runtime_object_graph(&index, objects.as_ref(), &standard_reference_registry()?)
            .expect_err("forged reference must fail");
    assert!(error.to_string().contains("semantic closure"));
    Ok(())
}

#[test]
fn rejects_forged_exported_port_summary() -> Result<()> {
    let (objects, mut index) = fixture()?;
    index.exported_ports[0].role = "client".to_owned();
    let error =
        validate_runtime_object_graph(&index, objects.as_ref(), &standard_reference_registry()?)
            .expect_err("forged port summary must fail");
    assert!(error.to_string().contains("exported Ports"));
    Ok(())
}

#[test]
fn rejects_corrupted_downloaded_object() -> Result<()> {
    let (objects, index) = fixture()?;
    let mut source = source(objects.as_ref(), &index)?;
    source.corrupt = Some(index.root_computation_ref.clone());
    let work = tempfile::tempdir()?;
    let error = download_and_validate_graph(
        &source,
        &GraphDownloadExpectation {
            index_digest: index.digest()?,
            root_computation_ref: index.root_computation_ref.clone(),
            object_count: index.objects.len(),
            logical_bytes: index.logical_bytes()?,
        },
        work.path(),
    )
    .err()
    .ok_or_else(|| anyhow::anyhow!("corrupted object unexpectedly passed"))?;
    assert!(error.to_string().contains("digest mismatch"));
    Ok(())
}

fn artifact(role: ArtifactRole, bytes: &[u8], objects: &dyn ObjectStore) -> Result<VmArtifact> {
    let reference = objects.put(bytes)?;
    Ok(VmArtifact {
        role,
        logical_size: bytes.len() as u64,
        chunks: vec![VmArtifactChunk {
            ordinal: 0,
            offset: 0,
            content_ref: reference.to_string(),
            size: bytes.len() as u64,
        }],
    })
}

fn vm_index(
    objects: &Arc<MemoryObjectStore>,
    base: &ObjectGraphIndexV1,
    target: String,
    frontier: ContentRef,
) -> Result<ObjectGraphIndexV1> {
    let layout = FirecrackerRestoreLayout::default();
    let descriptor = VmSnapshotDescriptor {
        version: 1,
        target_computation_ref: target,
        record_frontier_ref: Some(frontier.to_string()),
        backend: "firecracker".to_owned(),
        snapshot_format: "fc-vmstate-v1".to_owned(),
        architecture: "x86_64".to_owned(),
        guest_os: "linux".to_owned(),
        host_backend_contract: HostBackendContract {
            backend_id: "firecracker".to_owned(),
            host_os: "linux".to_owned(),
            required_features: BTreeSet::new(),
        },
        cpu_contract: CpuContract {
            vcpu_count: 1,
            required_features: BTreeSet::new(),
        },
        firecracker_version: "1.16.0".to_owned(),
        device_contract: DeviceContract {
            required_features: BTreeSet::new(),
        },
        network_contract: NetworkContract {
            required_features: BTreeSet::new(),
            tap_device: None,
        },
        vsock_contract: VsockContract {
            required_features: BTreeSet::new(),
            uds_path: layout.vsock_uds_path.clone(),
        },
        memory_contract: MemoryContract {
            guest_memory_mib: 128,
            minimum_host_memory_mib: 256,
        },
        artifacts: vec![
            artifact(ArtifactRole::Memory, b"memory", objects.as_ref())?,
            artifact(ArtifactRole::Rootfs, b"rootfs", objects.as_ref())?,
            artifact(ArtifactRole::Vmstate, b"vmstate", objects.as_ref())?,
            artifact(ArtifactRole::Metadata, &layout.encode()?, objects.as_ref())?,
        ],
        state_contract_refs: Vec::new(),
        contracts: Vec::new(),
        capture_provenance: CaptureProvenance {
            captured_at: "2026-08-24T00:00:00Z".to_owned(),
            backend_implementation_id: "test".to_owned(),
            source_realization_id: "realization-test".to_owned(),
            capture_barrier_complete: true,
            realization_quiesced: true,
            placement_hint: None,
        },
    };
    let descriptor_ref = objects.put(&serde_jcs::to_vec(&descriptor)?)?;
    let root = ComputationRef::parse(&base.root_computation_ref)?;
    let materializations = vec![GraphMaterialization {
        id: VM_SNAPSHOT_MATERIALIZER_ID.to_owned(),
        descriptor_ref: descriptor_ref.to_string(),
        restore_capability: GraphRestoreCapability::Supported,
    }];
    let closure = export_object_graph(
        &root,
        &materializations,
        objects.as_ref(),
        &standard_reference_registry()?,
    )?;
    Ok(ObjectGraphIndexV1::new(
        closure,
        base.exported_ports.clone(),
        base.required_bindings.clone(),
        VisibilityPolicy::Private,
    ))
}

fn frontier(objects: &dyn ObjectStore, observed: u64) -> Result<ContentRef> {
    objects
        .put(&serde_jcs::to_vec(&serde_json::json!({
            "version": 1,
            "run_id": "validator-test",
            "sealed_segments": [],
            "last_writer_order": 0,
            "observed_through": if observed == 0 {
                serde_json::json!({})
            } else {
                serde_json::json!({"pty.main": observed})
            },
            "causal_cut": []
        }))?)
        .map_err(Into::into)
}

#[test]
fn rejects_forged_vm_descriptor_target() -> Result<()> {
    let (objects, base) = fixture()?;
    let original = resolve_computation(
        objects.as_ref(),
        &ComputationRef::parse(&base.root_computation_ref)?,
    )?;
    let other_state = objects.put(&serde_jcs::to_vec(&serde_json::json!({
        "version": 1,
        "config": {"binding": []},
        "workspace_snapshot": load_authoring_state(&original, objects.as_ref())?.workspace_snapshot,
        "nonce": "different-existing-computation"
    }))?)?;
    let other_object = ComputationObject {
        semantics: original.object().semantics.clone(),
        boundary: original.object().boundary.clone(),
        residual: other_state,
    };
    let other = computation_ref(&other_object)?;
    objects.insert(
        other.content_ref(),
        &encode_computation_object(&other_object)?,
    )?;
    let index = vm_index(
        &objects,
        &base,
        other.to_string(),
        frontier(objects.as_ref(), 0)?,
    )?;
    let error =
        validate_runtime_object_graph(&index, objects.as_ref(), &standard_reference_registry()?)
            .expect_err("VM descriptor target mismatch must fail");
    assert!(error.to_string().contains("target does not match"));
    Ok(())
}

#[test]
fn rejects_forged_record_frontier_watermark() -> Result<()> {
    let (objects, base) = fixture()?;
    let index = vm_index(
        &objects,
        &base,
        base.root_computation_ref.clone(),
        frontier(objects.as_ref(), 99)?,
    )?;
    let error =
        validate_runtime_object_graph(&index, objects.as_ref(), &standard_reference_registry()?)
            .expect_err("forged RecordFrontier must fail");
    assert!(error.to_string().contains("RecordFrontier closure"));
    Ok(())
}

#[test]
fn rejects_missing_record_frontier_closure() -> Result<()> {
    let (objects, base) = fixture()?;
    let frontier = frontier(objects.as_ref(), 0)?;
    let mut index = vm_index(
        &objects,
        &base,
        base.root_computation_ref.clone(),
        frontier.clone(),
    )?;
    index
        .objects
        .retain(|object| object.content_ref != frontier.to_string());
    let error =
        validate_runtime_object_graph(&index, objects.as_ref(), &standard_reference_registry()?)
            .expect_err("missing RecordFrontier closure must fail");
    assert!(
        error.to_string().contains("reference is missing")
            || error.to_string().contains("semantic closure")
    );
    Ok(())
}

#[test]
fn index_shape_rejects_missing_declared_object() -> Result<()> {
    let (_objects, mut index) = fixture()?;
    index.objects.push(ato_objects::GraphObjectDescriptor {
        content_ref: ContentRef::parse(
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )?
        .to_string(),
        size_bytes: 1,
        kind: GraphObjectKind::Payload,
        references: vec![
            "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        ],
    });
    match validate_index_shape(&index) {
        Err(error) if error.to_string().contains("reference is missing") => Ok(()),
        Err(error) => bail!("unexpected error: {error:#}"),
        Ok(()) => bail!("missing declared object unexpectedly passed"),
    }
}
