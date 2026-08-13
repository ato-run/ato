use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use capsule_protocol::{
    CURRENT_COMPUTATION_SCHEMA_VERSION, ComputationDescriptor, ComputationRef, ComputationTypeId,
    ContentRef, LEGACY_STATE_IO_COMPUTATION_TYPE, LEGACY_STATE_IO_OBJECT_SCHEMA,
    LegacyStateIoComputationV1, PortDef, PortId, PortMode,
};
use serde::Serialize;

use super::{ProtocolBundleError, SpoolBundle};

/// A v1 Bundle projected into the computation model without changing the v1
/// descriptor or record stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLegacyComputation {
    pub descriptor: ComputationDescriptor,
    pub compatibility_object: LegacyStateIoComputationV1,
    pub cas_root: PathBuf,
}

#[derive(Serialize)]
struct LegacyStateIoObjectWire<'a> {
    schema: &'a str,
    descriptor_ref: &'a str,
    record_stream_ref: &'a str,
}

/// Stores the exact v1 descriptor and record members and a canonical
/// canonical compatibility object in a session-local CAS, then returns the
/// computation identity used by higher runtime layers.
///
/// Connector IDs are projected one-for-one to Port IDs. A v1 Connector does
/// not declare directionality, so the compatibility Port is necessarily
/// duplex; native descriptors must declare their narrower Port mode directly.
pub fn normalize_v1_spool(
    spool: &SpoolBundle,
    cas_root: &Path,
) -> Result<NormalizedLegacyComputation, ProtocolBundleError> {
    let descriptor_ref = store_file(cas_root, spool.descriptor_member.path())?;
    let record_stream_ref = store_file(cas_root, spool.records.path())?;

    let compatibility_object = LegacyStateIoComputationV1 {
        schema: LEGACY_STATE_IO_OBJECT_SCHEMA.to_owned(),
        descriptor_ref,
        record_stream_ref,
    };
    let object_bytes = serde_jcs::to_vec(&LegacyStateIoObjectWire {
        schema: &compatibility_object.schema,
        descriptor_ref: compatibility_object.descriptor_ref.as_str(),
        record_stream_ref: compatibility_object.record_stream_ref.as_str(),
    })
    .map_err(|error| {
        ProtocolBundleError::Invalid(format!(
            "failed to canonicalize legacy computation object: {error}"
        ))
    })?;
    let computation_ref = store_bytes(cas_root, &object_bytes)?;
    let computation_type = ComputationTypeId::parse(LEGACY_STATE_IO_COMPUTATION_TYPE)
        .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;

    let ports = spool
        .descriptor
        .connectors
        .iter()
        .map(|(connector_id, connector)| {
            let port_id = PortId::parse(connector_id.as_str())
                .expect("ConnectorId and PortId share validation rules");
            (
                port_id,
                PortDef {
                    protocol: connector.protocol.clone(),
                    mode: PortMode::Duplex,
                    config_ref: connector.config_ref.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();

    let descriptor = ComputationDescriptor {
        schema_version: CURRENT_COMPUTATION_SCHEMA_VERSION,
        root: ComputationRef {
            computation_type,
            computation_ref,
        },
        ports,
        trace_from: None,
    };
    descriptor
        .validate()
        .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;

    Ok(NormalizedLegacyComputation {
        descriptor,
        compatibility_object,
        cas_root: cas_root.to_path_buf(),
    })
}

fn store_file(cas_root: &Path, source: &Path) -> Result<ContentRef, ProtocolBundleError> {
    let mut bytes = Vec::new();
    File::open(source)?.read_to_end(&mut bytes)?;
    store_bytes(cas_root, &bytes)
}

fn store_bytes(cas_root: &Path, bytes: &[u8]) -> Result<ContentRef, ProtocolBundleError> {
    let digest = blake3::hash(bytes).to_hex().to_string();
    let reference = ContentRef::parse(format!("blake3:{digest}"))
        .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;
    let directory = cas_root.join(reference.algorithm());
    fs::create_dir_all(&directory)?;
    set_owner_only_directory(&directory)?;
    let destination = directory.join(reference.digest());
    if destination.exists() {
        let mut existing = Vec::new();
        File::open(&destination)?.read_to_end(&mut existing)?;
        if existing != bytes {
            return Err(ProtocolBundleError::Invalid(format!(
                "CAS object {reference} has conflicting bytes"
            )));
        }
        return Ok(reference);
    }

    let mut temporary = tempfile::Builder::new()
        .prefix(".legacy-computation-")
        .tempfile_in(&directory)?;
    set_owner_only_file(temporary.as_file())?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = Vec::new();
            File::open(&destination)?.read_to_end(&mut existing)?;
            if existing != bytes {
                return Err(ProtocolBundleError::Invalid(format!(
                    "CAS object {reference} has conflicting bytes"
                )));
            }
        }
        Err(error) => return Err(ProtocolBundleError::Io(error.error)),
    }
    Ok(reference)
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), ProtocolBundleError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), ProtocolBundleError> {
    Err(ProtocolBundleError::Invalid(
        "legacy computation CAS requires an owner-only filesystem backend".to_owned(),
    ))
}

#[cfg(unix)]
fn set_owner_only_file(file: &File) -> Result<(), ProtocolBundleError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_file(_file: &File) -> Result<(), ProtocolBundleError> {
    Err(ProtocolBundleError::Invalid(
        "legacy computation CAS requires an owner-only filesystem backend".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use capsule_codec::{encode_descriptor, encode_record_stream};
    use capsule_protocol::{
        CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef, ConnectorId, Direction, IoRecord,
        Payload, ProtocolId, RecordKindId, StateRef, StateTypeId,
    };

    use super::*;
    use crate::protocol_bundle::{PortableCapsule, StreamingBundleReader};

    fn fixture() -> PortableCapsule {
        let state_bytes = b"state".to_vec();
        let state_ref =
            ContentRef::parse(format!("blake3:{}", blake3::hash(&state_bytes).to_hex())).unwrap();
        let descriptor = CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.workspace-posix-host@1").unwrap(),
                state_ref: state_ref.clone(),
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("terminal.main").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        PortableCapsule {
            descriptor,
            records: vec![IoRecord {
                seq: 7,
                offset_ns: None,
                observed_at_unix_ns: None,
                connector: ConnectorId::parse("terminal.main").unwrap(),
                direction: Direction::Ingress,
                kind: RecordKindId::parse("stdin").unwrap(),
                payload: Payload::Inline(b"Alice\n".to_vec()),
            }],
            objects: BTreeMap::from([(state_ref, state_bytes)]),
        }
    }

    #[test]
    fn normalizes_v1_without_changing_its_wire_members() {
        let root = tempfile::tempdir().unwrap();
        let bundle_path = root.path().join("fixture.capsule");
        let capsule = fixture();
        let descriptor_before = encode_descriptor(&capsule.descriptor).unwrap();
        let records_before = encode_record_stream(&capsule.descriptor, &capsule.records).unwrap();
        capsule.write(&bundle_path).unwrap();

        let spool = StreamingBundleReader::read_into(&bundle_path, root.path()).unwrap();
        let normalized = normalize_v1_spool(&spool, &root.path().join("cas")).unwrap();

        let descriptor_object = fs::read(
            root.path()
                .join("cas/blake3")
                .join(normalized.compatibility_object.descriptor_ref.digest()),
        )
        .unwrap();
        let records_object = fs::read(
            root.path()
                .join("cas/blake3")
                .join(normalized.compatibility_object.record_stream_ref.digest()),
        )
        .unwrap();
        assert_eq!(descriptor_object, descriptor_before);
        assert_eq!(records_object, records_before);
        assert_eq!(
            normalized.descriptor.root.computation_type.as_str(),
            LEGACY_STATE_IO_COMPUTATION_TYPE
        );
        assert_eq!(
            normalized
                .descriptor
                .ports
                .get(&PortId::parse("terminal.main").unwrap())
                .unwrap()
                .mode,
            PortMode::Duplex
        );
    }

    #[test]
    fn normalization_is_content_addressed_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let bundle_path = root.path().join("fixture.capsule");
        fixture().write(&bundle_path).unwrap();
        let spool = StreamingBundleReader::read_into(&bundle_path, root.path()).unwrap();
        let cas = root.path().join("cas");

        let first = normalize_v1_spool(&spool, &cas).unwrap();
        let second = normalize_v1_spool(&spool, &cas).unwrap();
        assert_eq!(first.descriptor.root, second.descriptor.root);
        assert_eq!(first.compatibility_object, second.compatibility_object);
    }
}
