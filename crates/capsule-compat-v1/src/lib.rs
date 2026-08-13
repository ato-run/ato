//! Opaque adapter from Capsule Protocol v1 to the Computation Core.
//!
//! State, Connector, and Record semantics remain v1 concerns. This adapter
//! stores the exact descriptor and Record member bytes, projects only the open
//! boundary required for composition, and includes that boundary inside the
//! content-addressed Computation Object.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use capsule_core::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
    SemanticsId,
};
use capsule_core_codec::{ResolvedComputation, computation_ref, encode_computation_object};
use capsule_protocol::CapsuleDescriptor;
use serde::Serialize;
use thiserror::Error;

const CAS_COPY_BUFFER_BYTES: usize = 64 * 1024;

pub const LEGACY_V1_SEMANTICS: &str = "capsule.legacy-v1@1";
pub const LEGACY_V1_PORT_ROLE: &str = "legacy-peer";

#[derive(Debug, Clone, Copy)]
pub struct SpoolMember<'a> {
    pub path: &'a Path,
    pub size: u64,
}

/// Minimal v1 input boundary. The adapter does not depend on a Bundle runtime
/// or materialize v1 Records.
#[derive(Debug, Clone, Copy)]
pub struct LegacyV1Spool<'a> {
    pub descriptor: &'a CapsuleDescriptor,
    pub descriptor_member: SpoolMember<'a>,
    pub record_stream_member: SpoolMember<'a>,
}

/// Opaque type-defined body for the legacy v1 computation evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyV1Residual {
    pub descriptor_ref: ContentRef,
    pub record_stream_ref: ContentRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLegacyV1Computation {
    pub computation: ResolvedComputation,
    pub residual: LegacyV1Residual,
    pub cas_root: PathBuf,
}

#[derive(Debug, Error)]
pub enum CompatibilityError {
    #[error("v1 compatibility I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid v1 compatibility computation: {0}")]
    Invalid(String),
}

#[derive(Serialize)]
struct LegacyV1ResidualWire<'a> {
    descriptor_ref: &'a str,
    record_stream_ref: &'a str,
}

pub fn normalize_v1_spool(
    spool: LegacyV1Spool<'_>,
    cas_root: &Path,
) -> Result<NormalizedLegacyV1Computation, CompatibilityError> {
    let descriptor_ref = store_file(
        cas_root,
        spool.descriptor_member.path,
        spool.descriptor_member.size,
    )?;
    let record_stream_ref = store_file(
        cas_root,
        spool.record_stream_member.path,
        spool.record_stream_member.size,
    )?;
    let residual = LegacyV1Residual {
        descriptor_ref,
        record_stream_ref,
    };
    let residual_bytes = serde_jcs::to_vec(&LegacyV1ResidualWire {
        descriptor_ref: residual.descriptor_ref.as_str(),
        record_stream_ref: residual.record_stream_ref.as_str(),
    })
    .map_err(|error| {
        CompatibilityError::Invalid(format!("failed to encode v1 residual: {error}"))
    })?;
    let residual_ref = store_bytes(cas_root, &residual_bytes)?;

    let boundary = project_boundary(spool.descriptor)?;
    let object = ComputationObject {
        semantics: SemanticsId::parse(LEGACY_V1_SEMANTICS)
            .map_err(|error| CompatibilityError::Invalid(error.to_string()))?,
        boundary,
        residual: residual_ref,
    };
    let object_bytes = encode_computation_object(&object)
        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
    let expected_reference =
        computation_ref(&object).map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
    let stored_reference = ComputationRef::parse(store_bytes(cas_root, &object_bytes)?.to_string())
        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
    if stored_reference != expected_reference {
        return Err(CompatibilityError::Invalid(
            "canonical computation identity differs from stored object".to_owned(),
        ));
    }
    let computation = ResolvedComputation::verify(stored_reference, &object_bytes)
        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;

    Ok(NormalizedLegacyV1Computation {
        computation,
        residual,
        cas_root: cas_root.to_path_buf(),
    })
}

fn project_boundary(descriptor: &CapsuleDescriptor) -> Result<Boundary, CompatibilityError> {
    descriptor
        .connectors
        .iter()
        .map(|(connector_id, connector)| {
            let port_id = PortId::parse(connector_id.as_str())
                .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
            let protocol = ProtocolId::parse(connector.protocol.as_str())
                .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
            Ok((
                port_id,
                PortDef {
                    protocol,
                    role: RoleId::parse(LEGACY_V1_PORT_ROLE)
                        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?,
                },
            ))
        })
        .collect::<Result<Boundary, CompatibilityError>>()
}

fn store_file(
    cas_root: &Path,
    source: &Path,
    expected_size: u64,
) -> Result<ContentRef, CompatibilityError> {
    store_reader(cas_root, &mut File::open(source)?, expected_size)
}

fn store_reader(
    cas_root: &Path,
    source: &mut dyn Read,
    expected_size: u64,
) -> Result<ContentRef, CompatibilityError> {
    let directory = cas_root.join("blake3");
    fs::create_dir_all(&directory)?;
    set_owner_only_directory(&directory)?;

    let mut temporary = tempfile::Builder::new()
        .prefix(".legacy-v1-computation-")
        .tempfile_in(&directory)?;
    set_owner_only_file(temporary.as_file())?;
    let mut hasher = blake3::Hasher::new();
    let mut copied = 0_u64;
    let mut chunk = [0_u8; CAS_COPY_BUFFER_BYTES];
    loop {
        let count = source.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        temporary.write_all(&chunk[..count])?;
        hasher.update(&chunk[..count]);
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| CompatibilityError::Invalid("protocol member size overflow".into()))?;
        if copied > expected_size {
            return Err(CompatibilityError::Invalid(format!(
                "protocol member size mismatch: validated {expected_size} bytes, imported more than {expected_size}"
            )));
        }
    }
    if copied != expected_size {
        return Err(CompatibilityError::Invalid(format!(
            "protocol member size mismatch: validated {expected_size} bytes, imported {copied}"
        )));
    }
    temporary.as_file_mut().sync_all()?;

    let reference = ContentRef::parse(format!("blake3:{}", hasher.finalize().to_hex()))
        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
    persist_verified(
        temporary,
        &directory.join(reference.digest()),
        &reference,
        copied,
    )?;
    Ok(reference)
}

fn store_bytes(cas_root: &Path, bytes: &[u8]) -> Result<ContentRef, CompatibilityError> {
    let reference = ContentRef::parse(format!("blake3:{}", blake3::hash(bytes).to_hex()))
        .map_err(|error| CompatibilityError::Invalid(error.to_string()))?;
    let directory = cas_root.join(reference.algorithm());
    fs::create_dir_all(&directory)?;
    set_owner_only_directory(&directory)?;
    let destination = directory.join(reference.digest());
    let mut temporary = tempfile::Builder::new()
        .prefix(".legacy-v1-computation-")
        .tempfile_in(&directory)?;
    set_owner_only_file(temporary.as_file())?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    persist_verified(temporary, &destination, &reference, bytes.len() as u64)?;
    Ok(reference)
}

fn persist_verified(
    temporary: tempfile::NamedTempFile,
    destination: &Path,
    reference: &ContentRef,
    expected_size: u64,
) -> Result<(), CompatibilityError> {
    match temporary.persist_noclobber(destination) {
        Ok(_) => sync_parent_directory(destination),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_existing_object(destination, reference, expected_size)
        }
        Err(error) => Err(CompatibilityError::Io(error.error)),
    }
}

fn validate_existing_object(
    path: &Path,
    reference: &ContentRef,
    expected_size: u64,
) -> Result<(), CompatibilityError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(CompatibilityError::Invalid(format!(
            "CAS object {reference} has conflicting size"
        )));
    }

    let mut file = open_existing_object(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut read = 0_u64;
    let mut chunk = [0_u8; CAS_COPY_BUFFER_BYTES];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        hasher.update(&chunk[..count]);
        read = read
            .checked_add(count as u64)
            .ok_or_else(|| CompatibilityError::Invalid("CAS object size overflow".into()))?;
    }
    if read != expected_size || hasher.finalize().to_hex().as_str() != reference.digest() {
        return Err(CompatibilityError::Invalid(format!(
            "CAS object {reference} has conflicting content"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn open_existing_object(path: &Path) -> Result<File, CompatibilityError> {
    use std::os::unix::fs::OpenOptionsExt;

    Ok(fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?)
}

#[cfg(not(unix))]
fn open_existing_object(path: &Path) -> Result<File, CompatibilityError> {
    Ok(File::open(path)?)
}

fn sync_parent_directory(path: &Path) -> Result<(), CompatibilityError> {
    let parent = path.parent().ok_or_else(|| {
        CompatibilityError::Invalid("CAS object has no parent directory".to_owned())
    })?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), CompatibilityError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), CompatibilityError> {
    Err(CompatibilityError::Invalid(
        "legacy v1 computation CAS requires an owner-only filesystem backend".to_owned(),
    ))
}

#[cfg(unix)]
fn set_owner_only_file(file: &File) -> Result<(), CompatibilityError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_file(_file: &File) -> Result<(), CompatibilityError> {
    Err(CompatibilityError::Invalid(
        "legacy v1 computation CAS requires an owner-only filesystem backend".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io;

    use capsule_codec::{encode_descriptor, encode_record_stream};
    use capsule_protocol::{
        CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef, ConnectorId, Direction, IoRecord,
        Payload, ProtocolId as V1ProtocolId, RecordKindId, StateRef, StateTypeId,
    };

    use super::*;

    fn fixture() -> (CapsuleDescriptor, Vec<IoRecord>) {
        let state_ref = capsule_protocol::ContentRef::parse(format!(
            "blake3:{}",
            blake3::hash(b"state").to_hex()
        ))
        .unwrap();
        let descriptor = CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.workspace-posix-host@1").unwrap(),
                state_ref,
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("terminal.main").unwrap(),
                ConnectorDef {
                    protocol: V1ProtocolId::parse("ato.io.pty@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        let records = vec![IoRecord {
            seq: 7,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Inline(b"Alice\n".to_vec()),
        }];
        (descriptor, records)
    }

    fn write_members(root: &Path) -> (CapsuleDescriptor, PathBuf, PathBuf) {
        let (descriptor, records) = fixture();
        let descriptor_path = root.join("descriptor.cbor");
        let records_path = root.join("records.cborseq");
        fs::write(&descriptor_path, encode_descriptor(&descriptor).unwrap()).unwrap();
        fs::write(
            &records_path,
            encode_record_stream(&descriptor, &records).unwrap(),
        )
        .unwrap();
        (descriptor, descriptor_path, records_path)
    }

    fn normalize_fixture(root: &Path) -> NormalizedLegacyV1Computation {
        let (descriptor, descriptor_path, records_path) = write_members(root);
        normalize_v1_spool(
            LegacyV1Spool {
                descriptor: &descriptor,
                descriptor_member: SpoolMember {
                    path: &descriptor_path,
                    size: fs::metadata(&descriptor_path).unwrap().len(),
                },
                record_stream_member: SpoolMember {
                    path: &records_path,
                    size: fs::metadata(&records_path).unwrap().len(),
                },
            },
            &root.join("cas"),
        )
        .unwrap()
    }

    #[test]
    fn normalization_keeps_v1_members_opaque_and_boundary_in_object() {
        let root = tempfile::tempdir().unwrap();
        let normalized = normalize_fixture(root.path());

        assert_eq!(
            normalized.computation.object().semantics.as_str(),
            LEGACY_V1_SEMANTICS
        );
        assert_eq!(
            normalized
                .computation
                .object()
                .boundary
                .get(&PortId::parse("terminal.main").unwrap())
                .unwrap()
                .role
                .as_str(),
            LEGACY_V1_PORT_ROLE
        );
        assert_ne!(
            normalized.computation.reference().content_ref(),
            &normalized.residual.descriptor_ref
        );
    }

    #[test]
    fn normalization_is_content_addressed_and_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let first = normalize_fixture(root.path());
        let second = normalize_fixture(root.path());

        assert_eq!(first.computation, second.computation);
        assert_eq!(first.residual, second.residual);
    }

    #[test]
    fn changing_boundary_changes_computation_identity() {
        let residual = ContentRef::parse(format!("blake3:{}", "aa".repeat(32))).unwrap();
        let mut first = ComputationObject {
            semantics: SemanticsId::parse("example.greeter@1").unwrap(),
            boundary: Boundary::default(),
            residual,
        };
        let first_ref = computation_ref(&first).unwrap();
        first.boundary.insert(
            PortId::parse("greeter.name").unwrap(),
            PortDef {
                protocol: ProtocolId::parse("example.greeter.text@1").unwrap(),
                role: RoleId::parse("server").unwrap(),
            },
        );
        let second_ref = computation_ref(&first).unwrap();

        assert_ne!(first_ref, second_ref);
    }

    struct LargeRecordMember {
        remaining: u64,
        largest_requested_chunk: usize,
    }

    impl Read for LargeRecordMember {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.largest_requested_chunk = self.largest_requested_chunk.max(buffer.len());
            let count = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap();
            buffer[..count].fill(0xa5);
            self.remaining -= count as u64;
            Ok(count)
        }
    }

    #[test]
    fn large_record_member_import_uses_bounded_chunks() {
        let root = tempfile::tempdir().unwrap();
        let size = 32 * 1024 * 1024;
        let mut records = LargeRecordMember {
            remaining: size,
            largest_requested_chunk: 0,
        };

        let reference = store_reader(root.path(), &mut records, size).unwrap();
        let stored = fs::metadata(
            root.path()
                .join(reference.algorithm())
                .join(reference.digest()),
        )
        .unwrap();

        assert_eq!(stored.len(), size);
        assert_eq!(records.largest_requested_chunk, CAS_COPY_BUFFER_BYTES);
    }

    #[test]
    fn protocol_member_import_rejects_validated_size_mismatch() {
        let root = tempfile::tempdir().unwrap();
        let mut records = io::Cursor::new(vec![0_u8; 17]);
        let error = store_reader(root.path(), &mut records, 18).unwrap_err();
        assert!(error.to_string().contains("size mismatch"));

        let mut records = io::Cursor::new(vec![0_u8; 19]);
        let error = store_reader(root.path(), &mut records, 18).unwrap_err();
        assert!(error.to_string().contains("size mismatch"));
    }
}
