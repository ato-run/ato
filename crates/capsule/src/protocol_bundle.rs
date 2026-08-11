//! Minimal self-contained bundle for Capsule Protocol State and I/O.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use capsule_codec::{
    decode_descriptor, decode_record_stream, encode_descriptor, encode_record_stream,
};
use capsule_protocol::{CapsuleDescriptor, ContentRef, IoRecord, StateRef, StateTypeId};
use thiserror::Error;

const DESCRIPTOR_MEMBER: &str = "protocol/descriptor.cbor";
const RECORDS_MEMBER: &str = "protocol/records.cborseq";
const OBJECT_PREFIX: &str = "objects/";
const MAX_MEMBER_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ProtocolBundleError {
    #[error("bundle I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("bundle codec failed: {0}")]
    Codec(#[from] capsule_codec::CodecError),
    #[error("invalid protocol bundle: {0}")]
    Invalid(String),
    #[error("workspace entry is unsupported: {0}")]
    UnsupportedWorkspaceEntry(PathBuf),
}

#[derive(Debug, Clone)]
pub struct PortableCapsule {
    pub descriptor: CapsuleDescriptor,
    pub records: Vec<IoRecord>,
    pub objects: BTreeMap<ContentRef, Vec<u8>>,
}

impl PortableCapsule {
    pub fn validate(&self) -> Result<(), ProtocolBundleError> {
        self.descriptor
            .validate()
            .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;
        encode_record_stream(&self.descriptor, &self.records)?;
        let state_bytes = self
            .objects
            .get(&self.descriptor.base_state.state_ref)
            .ok_or_else(|| {
                ProtocolBundleError::Invalid(format!(
                    "base state object {} is missing",
                    self.descriptor.base_state.state_ref
                ))
            })?;
        verify_content_ref(&self.descriptor.base_state.state_ref, state_bytes)?;
        for (reference, bytes) in &self.objects {
            verify_content_ref(reference, bytes)?;
        }
        Ok(())
    }

    pub fn write(&self, path: &Path) -> Result<(), ProtocolBundleError> {
        self.validate()?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let file = fs::File::create(path)?;
        let mut archive = tar::Builder::new(file);
        archive.mode(tar::HeaderMode::Deterministic);
        append_bytes(
            &mut archive,
            DESCRIPTOR_MEMBER,
            &encode_descriptor(&self.descriptor)?,
        )?;
        append_bytes(
            &mut archive,
            RECORDS_MEMBER,
            &encode_record_stream(&self.descriptor, &self.records)?,
        )?;
        for (reference, bytes) in &self.objects {
            let member = format!(
                "{OBJECT_PREFIX}{}/{}",
                reference.algorithm(),
                reference.digest()
            );
            append_bytes(&mut archive, &member, bytes)?;
        }
        archive.finish()?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<Self, ProtocolBundleError> {
        let mut descriptor_bytes = None;
        let mut record_bytes = None;
        let mut objects = BTreeMap::new();
        let mut archive = tar::Archive::new(fs::File::open(path)?);
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.size() > MAX_MEMBER_BYTES {
                return Err(ProtocolBundleError::Invalid(format!(
                    "member exceeds {MAX_MEMBER_BYTES}-byte limit"
                )));
            }
            let member_path = entry.path()?.into_owned();
            let member = member_path.to_str().ok_or_else(|| {
                ProtocolBundleError::Invalid("member path is not UTF-8".to_owned())
            })?;
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            match member {
                DESCRIPTOR_MEMBER => set_once(&mut descriptor_bytes, bytes, member)?,
                RECORDS_MEMBER => set_once(&mut record_bytes, bytes, member)?,
                _ if member.starts_with(OBJECT_PREFIX) => {
                    let suffix = &member[OBJECT_PREFIX.len()..];
                    let (algorithm, digest) = suffix.split_once('/').ok_or_else(|| {
                        ProtocolBundleError::Invalid(format!("invalid object member `{member}`"))
                    })?;
                    let reference = ContentRef::parse(format!("{algorithm}:{digest}"))
                        .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;
                    if objects.insert(reference, bytes).is_some() {
                        return Err(ProtocolBundleError::Invalid(format!(
                            "duplicate object member `{member}`"
                        )));
                    }
                }
                _ => {
                    return Err(ProtocolBundleError::Invalid(format!(
                        "unknown bundle member `{member}`"
                    )));
                }
            }
        }
        let descriptor = decode_descriptor(&descriptor_bytes.ok_or_else(|| {
            ProtocolBundleError::Invalid(format!("missing `{DESCRIPTOR_MEMBER}`"))
        })?)?;
        let records = decode_record_stream(
            &descriptor,
            &record_bytes.ok_or_else(|| {
                ProtocolBundleError::Invalid(format!("missing `{RECORDS_MEMBER}`"))
            })?,
        )?;
        let bundle = Self {
            descriptor,
            records,
            objects,
        };
        bundle.validate()?;
        Ok(bundle)
    }
}

pub fn capture_workspace_state(
    workspace: &Path,
) -> Result<(StateRef, Vec<u8>), ProtocolBundleError> {
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(ProtocolBundleError::Invalid(format!(
            "workspace is not a directory: {}",
            workspace.display()
        )));
    }
    let mut entries = Vec::new();
    collect_workspace_entries(&workspace, &workspace, &mut entries)?;
    entries.sort();

    let mut archive = tar::Builder::new(Vec::new());
    archive.mode(tar::HeaderMode::Deterministic);
    for relative in entries {
        let source = workspace.join(&relative);
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() {
            return Err(ProtocolBundleError::UnsupportedWorkspaceEntry(relative));
        }
        if metadata.is_dir() {
            append_directory(&mut archive, &relative)?;
        } else if metadata.is_file() {
            append_file(&mut archive, &relative, &source, &metadata)?;
        } else {
            return Err(ProtocolBundleError::UnsupportedWorkspaceEntry(relative));
        }
    }
    let bytes = archive.into_inner()?;
    let reference = content_ref(&bytes);
    Ok((
        StateRef {
            state_type: StateTypeId::parse("ato.state.workspace-posix@1")
                .expect("static state type"),
            state_ref: reference,
        },
        bytes,
    ))
}

pub fn restore_workspace_state(
    state: &StateRef,
    object: &[u8],
    destination: &Path,
) -> Result<(), ProtocolBundleError> {
    if state.state_type.as_str() != "ato.state.workspace-posix@1" {
        return Err(ProtocolBundleError::Invalid(format!(
            "unsupported state type {}",
            state.state_type
        )));
    }
    verify_content_ref(&state.state_ref, object)?;
    if destination.exists() && destination.read_dir()?.next().is_some() {
        return Err(ProtocolBundleError::Invalid(format!(
            "restore destination is not empty: {}",
            destination.display()
        )));
    }
    fs::create_dir_all(destination)?;
    let mut archive = tar::Archive::new(Cursor::new(object));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        validate_relative_path(&path)?;
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Directory
        ) {
            return Err(ProtocolBundleError::Invalid(format!(
                "unsupported state member type for `{}`",
                path.display()
            )));
        }
        entry.unpack_in(destination)?;
    }
    Ok(())
}

pub fn content_ref(bytes: &[u8]) -> ContentRef {
    ContentRef::parse(format!("blake3:{}", blake3::hash(bytes).to_hex()))
        .expect("BLAKE3 always produces a valid content ref")
}

fn collect_workspace_entries(
    root: &Path,
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), ProtocolBundleError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if directory == root && (name == ".git" || name == ".ato") {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?
            .to_path_buf();
        output.push(relative);
        if entry.file_type()?.is_dir() {
            collect_workspace_entries(root, &path, output)?;
        }
    }
    Ok(())
}

fn append_bytes(
    archive: &mut tar::Builder<fs::File>,
    path: &str,
    bytes: &[u8],
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    normalize_header(
        &mut header,
        bytes.len() as u64,
        0o644,
        tar::EntryType::Regular,
    );
    archive.append_data(&mut header, path, Cursor::new(bytes))
}

fn append_directory(
    archive: &mut tar::Builder<Vec<u8>>,
    path: &Path,
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    normalize_header(&mut header, 0, 0o755, tar::EntryType::Directory);
    archive.append_data(&mut header, path, std::io::empty())
}

fn append_file(
    archive: &mut tar::Builder<Vec<u8>>,
    relative: &Path,
    source: &Path,
    metadata: &fs::Metadata,
) -> Result<(), std::io::Error> {
    let mode = file_mode(metadata);
    let mut header = tar::Header::new_gnu();
    normalize_header(&mut header, metadata.len(), mode, tar::EntryType::Regular);
    archive.append_data(&mut header, relative, fs::File::open(source)?)
}

fn normalize_header(header: &mut tar::Header, size: u64, mode: u32, kind: tar::EntryType) {
    header.set_size(size);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(kind);
    header.set_cksum();
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

fn verify_content_ref(reference: &ContentRef, bytes: &[u8]) -> Result<(), ProtocolBundleError> {
    let actual = match reference.algorithm() {
        "blake3" => blake3::hash(bytes).to_hex().to_string(),
        "sha256" => {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(bytes))
        }
        _ => unreachable!("ContentRef constructor restricts algorithms"),
    };
    if actual != reference.digest() {
        return Err(ProtocolBundleError::Invalid(format!(
            "object digest mismatch for {reference}"
        )));
    }
    Ok(())
}

fn set_once(
    slot: &mut Option<Vec<u8>>,
    value: Vec<u8>,
    member: &str,
) -> Result<(), ProtocolBundleError> {
    if slot.replace(value).is_some() {
        return Err(ProtocolBundleError::Invalid(format!(
            "duplicate `{member}`"
        )));
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), ProtocolBundleError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ProtocolBundleError::Invalid(format!(
            "unsafe state member `{}`",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_protocol::{CURRENT_SCHEMA_VERSION, ConnectorDef, ConnectorId, ProtocolId};

    #[test]
    fn workspace_state_round_trips_without_original_directory() {
        let producer = tempfile::tempdir().unwrap();
        fs::write(producer.path().join("main.rs"), "fn main() {}\n").unwrap();
        let (state, object) = capture_workspace_state(producer.path()).unwrap();
        drop(producer);

        let consumer = tempfile::tempdir().unwrap();
        let destination = consumer.path().join("restored");
        restore_workspace_state(&state, &object, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    #[test]
    fn portable_bundle_round_trips_and_verifies_objects() {
        let producer = tempfile::tempdir().unwrap();
        fs::write(producer.path().join("main.rs"), "fn main() {}\n").unwrap();
        let (state, object) = capture_workspace_state(producer.path()).unwrap();
        let state_ref = state.state_ref.clone();
        let bundle = PortableCapsule {
            descriptor: CapsuleDescriptor {
                schema_version: CURRENT_SCHEMA_VERSION,
                base_state: state,
                connectors: BTreeMap::from([(
                    ConnectorId::parse("terminal.main").unwrap(),
                    ConnectorDef {
                        protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                        config_ref: None,
                    },
                )]),
            },
            records: Vec::new(),
            objects: BTreeMap::from([(state_ref, object)]),
        };
        let output = producer.path().join("portable.capsule");
        bundle.write(&output).unwrap();
        let decoded = PortableCapsule::read(&output).unwrap();
        assert_eq!(decoded.descriptor, bundle.descriptor);
        assert_eq!(decoded.objects, bundle.objects);
    }

    #[test]
    fn restore_rejects_link_entries_even_when_object_digest_is_valid() {
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        normalize_header(&mut header, 0, 0o777, tar::EntryType::Symlink);
        header.set_link_name("../outside").unwrap();
        header.set_cksum();
        archive
            .append_data(&mut header, "link", std::io::empty())
            .unwrap();
        let object = archive.into_inner().unwrap();
        let state = StateRef {
            state_type: StateTypeId::parse("ato.state.workspace-posix@1").unwrap(),
            state_ref: content_ref(&object),
        };
        let destination = tempfile::tempdir().unwrap();
        let error = restore_workspace_state(&state, &object, destination.path()).unwrap_err();
        assert!(error.to_string().contains("unsupported state member type"));
    }
}
