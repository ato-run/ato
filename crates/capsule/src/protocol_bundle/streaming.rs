use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capsule_codec::{
    RecordStreamDecoder, RecordStreamEncoder, decode_descriptor_reader, encode_descriptor,
};
use capsule_protocol::{
    CapsuleDescriptor, ConnectorId, ContentRef, Direction, IoRecord, Payload, ProtocolId,
    RecordKindId, StateTypeId,
};
use sha2::{Digest, Sha256};
use tempfile::{Builder as TempBuilder, NamedTempFile};
use thiserror::Error;

use super::{
    BUNDLE_LIMITS, DESCRIPTOR_MEMBER, MAX_ARCHIVE_BYTES, MAX_OBJECT_COUNT, MemberLimitValidator,
    OBJECT_PREFIX, ProtocolBundleError, RECORDS_MEMBER, normalize_header,
};
use crate::packers::pack_filter::PackFilter;
use crate::security::no_secret::{CredentialScanner, scan_credential_material};

const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub reference: ContentRef,
    pub size: u64,
}

pub trait ObjectSource {
    fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError>;

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send>, ProtocolBundleError>;
}

pub struct InMemoryObjectSource<'a> {
    objects: &'a BTreeMap<ContentRef, Vec<u8>>,
}

impl<'a> InMemoryObjectSource<'a> {
    pub fn new(objects: &'a BTreeMap<ContentRef, Vec<u8>>) -> Self {
        Self { objects }
    }
}

impl ObjectSource for InMemoryObjectSource<'_> {
    fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
        Ok(self
            .objects
            .iter()
            .map(|(reference, bytes)| {
                (
                    reference.clone(),
                    ObjectMetadata {
                        reference: reference.clone(),
                        size: bytes.len() as u64,
                    },
                )
            })
            .collect())
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
        let bytes = self.objects.get(reference).ok_or_else(|| {
            ProtocolBundleError::Invalid(format!("object source is missing {reference}"))
        })?;
        Ok(Box::new(Cursor::new(bytes.clone())))
    }
}

#[derive(Debug, Clone)]
pub struct DirectoryObjectSource {
    root: PathBuf,
}

impl DirectoryObjectSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, reference: &ContentRef) -> PathBuf {
        self.root
            .join(reference.algorithm())
            .join(reference.digest())
    }
}

impl ObjectSource for DirectoryObjectSource {
    fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
        let mut index = BTreeMap::new();
        for algorithm in ["blake3", "sha256"] {
            let directory = self.root.join(algorithm);
            if !directory.exists() {
                continue;
            }
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file() {
                    return Err(ProtocolBundleError::Invalid(format!(
                        "object source entry is not a regular file: {}",
                        entry.path().display()
                    )));
                }
                let digest = entry.file_name().into_string().map_err(|_| {
                    ProtocolBundleError::Invalid("object source path is not UTF-8".to_owned())
                })?;
                let reference = ContentRef::parse(format!("{algorithm}:{digest}"))
                    .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;
                index.insert(
                    reference.clone(),
                    ObjectMetadata {
                        reference,
                        size: metadata.len(),
                    },
                );
            }
        }
        Ok(index)
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
        let path = self.path_for(reference);
        let before = fs::symlink_metadata(&path)?;
        if !before.is_file() {
            return Err(ProtocolBundleError::Invalid(format!(
                "object source entry is not a regular file: {}",
                path.display()
            )));
        }
        let file = open_regular_file(&path)?;
        if !file.metadata()?.is_file() {
            return Err(ProtocolBundleError::Invalid(format!(
                "opened object source is not a regular file: {}",
                path.display()
            )));
        }
        Ok(Box::new(BufReader::new(file)))
    }
}

#[cfg(unix)]
fn open_regular_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_regular_file(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortableObjectRole {
    BaseState {
        state_type: StateTypeId,
    },
    ConnectorConfig {
        connector_id: ConnectorId,
        protocol_id: ProtocolId,
    },
    RecordPayload {
        seq: u64,
        connector_id: ConnectorId,
        kind: RecordKindId,
        direction: Direction,
    },
    StateAdapterObject {
        state_type: StateTypeId,
    },
}

#[derive(Debug, Error)]
pub enum PortableExportError {
    #[error("portable export policy is unavailable for connector protocol {0}")]
    PolicyUnavailable(ProtocolId),
    #[error("portable export policy is unavailable for State type {0}")]
    StatePolicyUnavailable(StateTypeId),
    #[error("portable export rejected unclassified object {0}")]
    UnclassifiedObject(ContentRef),
    #[error("portable export rejected credential material in {role} ({kind})")]
    CredentialMaterial { role: String, kind: &'static str },
    #[error("portable export policy rejected data: {0}")]
    Rejected(String),
    #[error("portable export policy I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub trait PortableExportPolicy {
    fn inspect_descriptor(
        &mut self,
        _descriptor: &CapsuleDescriptor,
    ) -> Result<(), PortableExportError> {
        Ok(())
    }

    fn inspect_record(&mut self, record: &IoRecord) -> Result<(), PortableExportError>;

    fn inspect_object(
        &mut self,
        metadata: &ObjectMetadata,
        roles: &[PortableObjectRole],
        reader: &mut dyn Read,
    ) -> Result<(), PortableExportError>;
}

/// Compatibility policy for the legacy in-memory API only.
pub struct AllowAllPortableExportPolicy;

impl PortableExportPolicy for AllowAllPortableExportPolicy {
    fn inspect_record(&mut self, _record: &IoRecord) -> Result<(), PortableExportError> {
        Ok(())
    }

    fn inspect_object(
        &mut self,
        _metadata: &ObjectMetadata,
        _roles: &[PortableObjectRole],
        _reader: &mut dyn Read,
    ) -> Result<(), PortableExportError> {
        Ok(())
    }
}

pub struct PtyPortableExportPolicy {
    protocols: BTreeMap<ConnectorId, ProtocolId>,
}

impl PtyPortableExportPolicy {
    pub fn new(descriptor: &CapsuleDescriptor) -> Self {
        Self {
            protocols: descriptor
                .connectors
                .iter()
                .map(|(id, definition)| (id.clone(), definition.protocol.clone()))
                .collect(),
        }
    }

    fn ensure_pty(&self, connector: &ConnectorId) -> Result<(), PortableExportError> {
        let protocol = self.protocols.get(connector).ok_or_else(|| {
            PortableExportError::Rejected(format!("undeclared connector {connector}"))
        })?;
        if protocol.as_str() != "ato.io.pty@1" {
            return Err(PortableExportError::PolicyUnavailable(protocol.clone()));
        }
        Ok(())
    }
}

impl PortableExportPolicy for PtyPortableExportPolicy {
    fn inspect_descriptor(
        &mut self,
        descriptor: &CapsuleDescriptor,
    ) -> Result<(), PortableExportError> {
        for connector in descriptor.connectors.values() {
            if connector.protocol.as_str() != "ato.io.pty@1" {
                return Err(PortableExportError::PolicyUnavailable(
                    connector.protocol.clone(),
                ));
            }
        }
        Ok(())
    }

    fn inspect_record(&mut self, record: &IoRecord) -> Result<(), PortableExportError> {
        self.ensure_pty(&record.connector)?;
        match (record.kind.as_str(), record.direction) {
            ("stdin", Direction::Ingress) | ("output", Direction::Egress) => {
                if let Payload::Inline(bytes) = &record.payload {
                    reject_findings(bytes, format!("record {}", record.seq))?;
                }
            }
            ("resize", Direction::Ingress) | ("exit", Direction::Egress) => {}
            (kind, direction) => {
                return Err(PortableExportError::Rejected(format!(
                    "invalid ato.io.pty@1 record kind/direction {kind}/{direction:?}"
                )));
            }
        }
        Ok(())
    }

    fn inspect_object(
        &mut self,
        _metadata: &ObjectMetadata,
        roles: &[PortableObjectRole],
        reader: &mut dyn Read,
    ) -> Result<(), PortableExportError> {
        let mut scan = false;
        for role in roles {
            match role {
                PortableObjectRole::RecordPayload {
                    connector_id, kind, ..
                } => {
                    self.ensure_pty(connector_id)?;
                    scan |= matches!(kind.as_str(), "stdin" | "output");
                }
                PortableObjectRole::ConnectorConfig { connector_id, .. } => {
                    self.ensure_pty(connector_id)?;
                    scan = true;
                }
                PortableObjectRole::BaseState { .. }
                | PortableObjectRole::StateAdapterObject { .. } => {}
            }
        }
        if scan {
            let mut scanner = CredentialScanner::new();
            let mut chunk = [0_u8; COPY_BUFFER_BYTES];
            loop {
                let count = reader.read(&mut chunk)?;
                if count == 0 {
                    break;
                }
                scanner.push(&chunk[..count]);
            }
            if let Some(finding) = scanner.finish().first() {
                return Err(PortableExportError::CredentialMaterial {
                    role: "object payload".to_owned(),
                    kind: finding.kind,
                });
            }
        }
        Ok(())
    }
}

/// Strict portable boundary policy for the currently supported workspace
/// State and PTY Connector contracts.
pub struct StrictPortableExportPolicy {
    connector_policy: PtyPortableExportPolicy,
}

impl StrictPortableExportPolicy {
    pub fn new(descriptor: &CapsuleDescriptor) -> Self {
        Self {
            connector_policy: PtyPortableExportPolicy::new(descriptor),
        }
    }
}

impl PortableExportPolicy for StrictPortableExportPolicy {
    fn inspect_descriptor(
        &mut self,
        descriptor: &CapsuleDescriptor,
    ) -> Result<(), PortableExportError> {
        ensure_workspace_state_policy(&descriptor.base_state.state_type)?;
        self.connector_policy.inspect_descriptor(descriptor)
    }

    fn inspect_record(&mut self, record: &IoRecord) -> Result<(), PortableExportError> {
        self.connector_policy.inspect_record(record)
    }

    fn inspect_object(
        &mut self,
        metadata: &ObjectMetadata,
        roles: &[PortableObjectRole],
        reader: &mut dyn Read,
    ) -> Result<(), PortableExportError> {
        if roles.is_empty() {
            return Err(PortableExportError::UnclassifiedObject(
                metadata.reference.clone(),
            ));
        }
        let mut workspace_base = false;
        let mut scan_raw = false;
        for role in roles {
            match role {
                PortableObjectRole::BaseState { state_type } => {
                    ensure_workspace_state_policy(state_type)?;
                    workspace_base = true;
                }
                PortableObjectRole::StateAdapterObject { state_type } => {
                    ensure_workspace_state_policy(state_type)?;
                    scan_raw = true;
                }
                PortableObjectRole::ConnectorConfig { .. }
                | PortableObjectRole::RecordPayload { .. } => scan_raw = true,
            }
        }
        if workspace_base {
            inspect_workspace_state_archive(reader)?;
        } else if scan_raw {
            scan_reader_for_credentials(reader, "object payload")?;
        }
        Ok(())
    }
}

fn ensure_workspace_state_policy(state_type: &StateTypeId) -> Result<(), PortableExportError> {
    if state_type.as_str() == "ato.state.workspace-posix-host@1" {
        Ok(())
    } else {
        Err(PortableExportError::StatePolicyUnavailable(
            state_type.clone(),
        ))
    }
}

fn scan_reader_for_credentials(
    reader: &mut dyn Read,
    role: &str,
) -> Result<(), PortableExportError> {
    let mut scanner = CredentialScanner::new();
    let mut chunk = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        scanner.push(&chunk[..count]);
    }
    if let Some(finding) = scanner.finish().first() {
        return Err(PortableExportError::CredentialMaterial {
            role: role.to_owned(),
            kind: finding.kind,
        });
    }
    Ok(())
}

fn inspect_workspace_state_archive(reader: &mut dyn Read) -> Result<(), PortableExportError> {
    let filter = PackFilter::for_portable_state()
        .map_err(|error| PortableExportError::Rejected(error.to_string()))?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Directory
        ) {
            return Err(PortableExportError::Rejected(
                "workspace State contains a non-file entry".to_owned(),
            ));
        }
        let path = strict_portable_state_path(entry.header())?;
        if !filter.should_include_file(&path)
            || (entry.header().entry_type() == tar::EntryType::Directory
                && !filter.should_include_file(&path.join("__ato_portable_probe__")))
        {
            return Err(PortableExportError::Rejected(format!(
                "workspace State contains non-portable path `{}`",
                path.display()
            )));
        }
        if entry.header().entry_type() == tar::EntryType::Regular {
            scan_reader_for_credentials(
                &mut entry,
                &format!("workspace State path `{}`", path.display()),
            )?;
        }
    }
    Ok(())
}

fn strict_portable_state_path(header: &tar::Header) -> Result<PathBuf, PortableExportError> {
    let bytes = header.path_bytes();
    let path = std::str::from_utf8(&bytes)
        .map_err(|_| PortableExportError::Rejected("State path is not UTF-8".to_owned()))?;
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(PortableExportError::Rejected(format!(
            "State path is not canonical: `{path}`"
        )));
    }
    Ok(PathBuf::from(path))
}

fn reject_findings(bytes: &[u8], role: String) -> Result<(), PortableExportError> {
    if let Some(finding) = scan_credential_material(bytes).first() {
        return Err(PortableExportError::CredentialMaterial {
            role,
            kind: finding.kind,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSummary {
    pub record_count: usize,
    pub record_bytes: u64,
    pub object_count: usize,
    pub object_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleWriteOutcome {
    pub path: PathBuf,
    pub summary: BundleSummary,
}

pub struct StreamingBundleWriter;

impl StreamingBundleWriter {
    pub fn write<R, O, P>(
        output: &Path,
        descriptor: &CapsuleDescriptor,
        records: R,
        objects: &O,
        policy: &mut P,
    ) -> Result<BundleWriteOutcome, ProtocolBundleError>
    where
        R: IntoIterator<Item = Result<IoRecord, ProtocolBundleError>>,
        O: ObjectSource,
        P: PortableExportPolicy,
    {
        Self::write_with_state_roles(
            output,
            descriptor,
            records,
            objects,
            &BTreeMap::new(),
            policy,
        )
    }

    pub fn write_with_state_roles<R, O, P>(
        output: &Path,
        descriptor: &CapsuleDescriptor,
        records: R,
        objects: &O,
        state_roles: &BTreeMap<ContentRef, Vec<PortableObjectRole>>,
        policy: &mut P,
    ) -> Result<BundleWriteOutcome, ProtocolBundleError>
    where
        R: IntoIterator<Item = Result<IoRecord, ProtocolBundleError>>,
        O: ObjectSource,
        P: PortableExportPolicy,
    {
        let descriptor_bytes = encode_descriptor(descriptor)?;
        let parent = output.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let mut record_temp = owner_only_tempfile(parent, ".records.cborseq-")?;
        let mut roles = direct_roles(descriptor);
        merge_state_roles(descriptor, &mut roles, state_roles)?;
        let record_stats = {
            let mut encoder = RecordStreamEncoder::new(descriptor, record_temp.as_file_mut())?;
            for record in records {
                let record = record?;
                if let Payload::Object(reference) = &record.payload {
                    roles.entry(reference.clone()).or_default().push(
                        PortableObjectRole::RecordPayload {
                            seq: record.seq,
                            connector_id: record.connector.clone(),
                            kind: record.kind.clone(),
                            direction: record.direction,
                        },
                    );
                }
                encoder.push(&record)?;
            }
            encoder.finish()?
        };
        record_temp.as_file_mut().sync_all()?;

        let index = objects.index()?;
        if index.len() > MAX_OBJECT_COUNT {
            return Err(ProtocolBundleError::Invalid(format!(
                "object count exceeds {MAX_OBJECT_COUNT}"
            )));
        }
        validate_index(&index)?;
        validate_closure(&roles, &index)?;

        let mut limits = MemberLimitValidator::new(BUNDLE_LIMITS);
        limits.accept(descriptor_bytes.len() as u64)?;
        limits.accept(record_stats.encoded_bytes)?;
        for metadata in index.values() {
            limits.accept(metadata.size)?;
        }

        policy.inspect_descriptor(descriptor)?;
        record_temp.as_file_mut().rewind()?;
        let mut decoder = RecordStreamDecoder::new(
            descriptor,
            BufReader::new(record_temp.as_file()),
            record_stats.encoded_bytes,
        )?;
        while let Some(record) = decoder.next_record()? {
            policy.inspect_record(&record)?;
        }
        for (reference, metadata) in &index {
            let mut reader = objects.open(reference)?;
            policy.inspect_object(
                metadata,
                roles.get(reference).map(Vec::as_slice).unwrap_or(&[]),
                reader.as_mut(),
            )?;
        }
        let prefix = format!(
            ".{}.tmp-",
            output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("capsule")
        );
        let mut output_temp = owner_only_tempfile(parent, &prefix)?;
        {
            let mut archive = tar::Builder::new(output_temp.as_file_mut());
            archive.mode(tar::HeaderMode::Deterministic);
            append_reader(
                &mut archive,
                DESCRIPTOR_MEMBER,
                descriptor_bytes.len() as u64,
                Cursor::new(&descriptor_bytes),
            )?;
            record_temp.as_file_mut().rewind()?;
            append_reader(
                &mut archive,
                RECORDS_MEMBER,
                record_stats.encoded_bytes,
                record_temp.as_file_mut(),
            )?;
            for (reference, metadata) in &index {
                let member = object_member(reference);
                let mut reader = objects.open(reference)?;
                append_verified_object(&mut archive, &member, metadata, reader.as_mut())?;
            }
            archive.finish()?;
        }
        output_temp.as_file_mut().sync_all()?;
        output_temp
            .persist(output)
            .map_err(|error| ProtocolBundleError::Io(error.error))?;
        sync_directory(parent)?;

        let object_bytes = index.values().try_fold(0_u64, |total, metadata| {
            total.checked_add(metadata.size).ok_or_else(|| {
                ProtocolBundleError::Invalid("aggregate object size overflow".to_owned())
            })
        })?;
        Ok(BundleWriteOutcome {
            path: output.to_path_buf(),
            summary: BundleSummary {
                record_count: record_stats.record_count,
                record_bytes: record_stats.encoded_bytes,
                object_count: index.len(),
                object_bytes,
            },
        })
    }
}

#[derive(Debug)]
struct SpoolGuard {
    root: PathBuf,
}

impl Drop for SpoolGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolMemberSpool {
    path: PathBuf,
    size: u64,
    _guard: Arc<SpoolGuard>,
}

impl ProtocolMemberSpool {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

#[derive(Debug, Clone)]
pub struct RecordSpool {
    path: PathBuf,
    size: u64,
    count: usize,
    _guard: Arc<SpoolGuard>,
}

impl RecordSpool {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn materialize(
        &self,
        descriptor: &CapsuleDescriptor,
    ) -> Result<Vec<IoRecord>, ProtocolBundleError> {
        let mut decoder = RecordStreamDecoder::new(
            descriptor,
            BufReader::new(File::open(&self.path)?),
            self.size,
        )?;
        let mut records = Vec::with_capacity(self.count);
        while let Some(record) = decoder.next_record()? {
            records.push(record);
        }
        Ok(records)
    }
}

#[derive(Debug, Clone)]
pub struct SpoolObjectStore {
    root: PathBuf,
    index: BTreeMap<ContentRef, ObjectMetadata>,
    _guard: Arc<SpoolGuard>,
}

impl SpoolObjectStore {
    pub fn index(&self) -> &BTreeMap<ContentRef, ObjectMetadata> {
        &self.index
    }

    pub fn open(&self, reference: &ContentRef) -> Result<File, ProtocolBundleError> {
        if !self.index.contains_key(reference) {
            return Err(ProtocolBundleError::Invalid(format!(
                "spool object is missing {reference}"
            )));
        }
        Ok(File::open(object_path(&self.root, reference))?)
    }

    pub fn materialize(&self) -> Result<BTreeMap<ContentRef, Vec<u8>>, ProtocolBundleError> {
        let mut objects = BTreeMap::new();
        for reference in self.index.keys() {
            let mut bytes = Vec::new();
            self.open(reference)?.read_to_end(&mut bytes)?;
            objects.insert(reference.clone(), bytes);
        }
        Ok(objects)
    }
}

impl ObjectSource for SpoolObjectStore {
    fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
        Ok(self.index.clone())
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
        Ok(Box::new(BufReader::new(self.open(reference)?)))
    }
}

#[derive(Debug)]
pub struct SpoolBundle {
    pub descriptor: CapsuleDescriptor,
    pub descriptor_member: ProtocolMemberSpool,
    pub records: RecordSpool,
    pub objects: SpoolObjectStore,
    pub summary: BundleSummary,
}

pub struct StreamingBundleReader;

impl StreamingBundleReader {
    pub fn read_into(
        bundle_path: &Path,
        spool_root: &Path,
    ) -> Result<SpoolBundle, ProtocolBundleError> {
        let archive_size = fs::metadata(bundle_path)?.len();
        if archive_size > MAX_ARCHIVE_BYTES {
            return Err(ProtocolBundleError::Invalid(format!(
                "archive exceeds {MAX_ARCHIVE_BYTES}-byte limit"
            )));
        }
        fs::create_dir_all(spool_root)?;
        let tempdir = TempBuilder::new()
            .prefix(".bundle-spool-")
            .tempdir_in(spool_root)?;
        set_owner_only_directory(tempdir.path())?;
        let root = tempdir.keep();
        let guard = Arc::new(SpoolGuard { root: root.clone() });

        let result = Self::read_staged(bundle_path, &root, guard.clone());
        if result.is_err() {
            drop(guard);
        }
        result
    }

    fn read_staged(
        bundle_path: &Path,
        root: &Path,
        guard: Arc<SpoolGuard>,
    ) -> Result<SpoolBundle, ProtocolBundleError> {
        let mut limits = MemberLimitValidator::new(BUNDLE_LIMITS);
        let mut archive = tar::Archive::new(File::open(bundle_path)?);
        archive.set_ignore_zeros(false);
        let mut descriptor = None;
        let mut record_spool = None;
        let mut roles = BTreeMap::new();
        let mut object_index = BTreeMap::new();
        let mut previous_object = None;
        for (position, entry) in archive.entries()?.raw(true).enumerate() {
            let mut entry = entry?;
            limits.accept(entry.size())?;
            validate_bundle_header(entry.header())?;
            let member = strict_member_path(entry.header())?;
            match position {
                0 if member == DESCRIPTOR_MEMBER => {
                    let size = entry.size();
                    let path = root.join("descriptor.cbor");
                    let copied = copy_exact(&mut entry, &mut owner_only_file(&path)?)?;
                    if copied != size {
                        return Err(ProtocolBundleError::Invalid(
                            "truncated descriptor member".to_owned(),
                        ));
                    }
                    let value = decode_descriptor_reader(BufReader::new(File::open(&path)?), size)?;
                    descriptor = Some((value, path, size));
                }
                1 if member == RECORDS_MEMBER => {
                    let descriptor_ref =
                        descriptor.as_ref().map(|value| &value.0).ok_or_else(|| {
                            ProtocolBundleError::Invalid("records precede descriptor".to_owned())
                        })?;
                    let path = root.join("records.cborseq");
                    let copied = copy_exact(&mut entry, &mut owner_only_file(&path)?)?;
                    if copied != entry.size() {
                        return Err(ProtocolBundleError::Invalid(
                            "truncated records member".to_owned(),
                        ));
                    }
                    let mut decoder = RecordStreamDecoder::new(
                        descriptor_ref,
                        BufReader::new(File::open(&path)?),
                        copied,
                    )?;
                    while let Some(record) = decoder.next_record()? {
                        if let Payload::Object(reference) = &record.payload {
                            roles
                                .entry(reference.clone())
                                .or_insert_with(Vec::new)
                                .push(PortableObjectRole::RecordPayload {
                                    seq: record.seq,
                                    connector_id: record.connector.clone(),
                                    kind: record.kind.clone(),
                                    direction: record.direction,
                                });
                        }
                    }
                    let stats = decoder.stats();
                    record_spool = Some((path, stats));
                }
                _ if position >= 2 && member.starts_with(OBJECT_PREFIX) => {
                    if object_index.len() >= MAX_OBJECT_COUNT {
                        return Err(ProtocolBundleError::Invalid(format!(
                            "object count exceeds {MAX_OBJECT_COUNT}"
                        )));
                    }
                    let reference = parse_object_member(&member)?;
                    if previous_object
                        .as_ref()
                        .is_some_and(|previous| previous >= &reference)
                    {
                        return Err(ProtocolBundleError::Invalid(format!(
                            "object members are not strictly sorted at {reference}"
                        )));
                    }
                    let path = object_path(root, &reference);
                    fs::create_dir_all(path.parent().expect("object path has parent"))?;
                    let mut temp = owner_only_tempfile(
                        path.parent().expect("object path has parent"),
                        ".object-",
                    )?;
                    let digest = copy_and_hash(&mut entry, temp.as_file_mut(), &reference)?;
                    if digest.bytes != entry.size() {
                        return Err(ProtocolBundleError::Invalid(format!(
                            "truncated object {reference}"
                        )));
                    }
                    verify_digest(&reference, &digest.hex)?;
                    temp.as_file_mut().sync_all()?;
                    temp.persist(&path)
                        .map_err(|error| ProtocolBundleError::Io(error.error))?;
                    object_index.insert(
                        reference.clone(),
                        ObjectMetadata {
                            reference: reference.clone(),
                            size: digest.bytes,
                        },
                    );
                    previous_object = Some(reference);
                }
                _ => {
                    return Err(ProtocolBundleError::Invalid(format!(
                        "unexpected bundle member `{member}` at position {position}"
                    )));
                }
            }
        }
        verify_archive_zero_tail(archive.into_inner())?;

        let (descriptor, descriptor_path, descriptor_size) = descriptor.ok_or_else(|| {
            ProtocolBundleError::Invalid(format!("missing `{DESCRIPTOR_MEMBER}`"))
        })?;
        let (record_path, record_stats) = record_spool
            .ok_or_else(|| ProtocolBundleError::Invalid(format!("missing `{RECORDS_MEMBER}`")))?;
        for (reference, object_roles) in direct_roles(&descriptor) {
            roles.entry(reference).or_default().extend(object_roles);
        }
        validate_closure(&roles, &object_index)?;
        let object_bytes = object_index.values().try_fold(0_u64, |total, metadata| {
            total.checked_add(metadata.size).ok_or_else(|| {
                ProtocolBundleError::Invalid("aggregate object size overflow".to_owned())
            })
        })?;
        Ok(SpoolBundle {
            descriptor,
            descriptor_member: ProtocolMemberSpool {
                path: descriptor_path,
                size: descriptor_size,
                _guard: guard.clone(),
            },
            records: RecordSpool {
                path: record_path,
                size: record_stats.encoded_bytes,
                count: record_stats.record_count,
                _guard: guard.clone(),
            },
            objects: SpoolObjectStore {
                root: root.to_path_buf(),
                index: object_index.clone(),
                _guard: guard,
            },
            summary: BundleSummary {
                record_count: record_stats.record_count,
                record_bytes: record_stats.encoded_bytes,
                object_count: object_index.len(),
                object_bytes,
            },
        })
    }
}

fn verify_archive_zero_tail(mut reader: File) -> Result<(), ProtocolBundleError> {
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(ProtocolBundleError::Invalid(
                "nonzero data follows TAR end blocks".to_owned(),
            ));
        }
    }
}

fn direct_roles(descriptor: &CapsuleDescriptor) -> BTreeMap<ContentRef, Vec<PortableObjectRole>> {
    let mut roles = BTreeMap::new();
    roles.insert(
        descriptor.base_state.state_ref.clone(),
        vec![PortableObjectRole::BaseState {
            state_type: descriptor.base_state.state_type.clone(),
        }],
    );
    for (connector_id, connector) in &descriptor.connectors {
        if let Some(reference) = &connector.config_ref {
            roles
                .entry(reference.clone())
                .or_insert_with(Vec::new)
                .push(PortableObjectRole::ConnectorConfig {
                    connector_id: connector_id.clone(),
                    protocol_id: connector.protocol.clone(),
                });
        }
    }
    roles
}

fn merge_state_roles(
    descriptor: &CapsuleDescriptor,
    roles: &mut BTreeMap<ContentRef, Vec<PortableObjectRole>>,
    state_roles: &BTreeMap<ContentRef, Vec<PortableObjectRole>>,
) -> Result<(), ProtocolBundleError> {
    for (reference, additions) in state_roles {
        if additions.is_empty() {
            return Err(ProtocolBundleError::Invalid(format!(
                "State adapter role list is empty for {reference}"
            )));
        }
        for role in additions {
            let PortableObjectRole::StateAdapterObject { state_type } = role else {
                return Err(ProtocolBundleError::Invalid(format!(
                    "non-State-adapter role supplied for {reference}"
                )));
            };
            if state_type != &descriptor.base_state.state_type {
                return Err(ProtocolBundleError::Invalid(format!(
                    "State adapter role type {state_type} does not match base State type {}",
                    descriptor.base_state.state_type
                )));
            }
        }
        roles
            .entry(reference.clone())
            .or_default()
            .extend(additions.iter().cloned());
    }
    Ok(())
}

fn validate_index(index: &BTreeMap<ContentRef, ObjectMetadata>) -> Result<(), ProtocolBundleError> {
    for (reference, metadata) in index {
        if reference != &metadata.reference {
            return Err(ProtocolBundleError::Invalid(format!(
                "object index key {reference} does not match metadata {}",
                metadata.reference
            )));
        }
    }
    Ok(())
}

fn validate_closure(
    roles: &BTreeMap<ContentRef, Vec<PortableObjectRole>>,
    index: &BTreeMap<ContentRef, ObjectMetadata>,
) -> Result<(), ProtocolBundleError> {
    for reference in roles.keys() {
        if !index.contains_key(reference) {
            return Err(ProtocolBundleError::Invalid(format!(
                "reachable object {reference} is missing"
            )));
        }
    }
    Ok(())
}

fn append_reader<R: Read>(
    archive: &mut tar::Builder<&mut File>,
    path: &str,
    size: u64,
    reader: R,
) -> Result<(), ProtocolBundleError> {
    let mut header = tar::Header::new_gnu();
    normalize_header(&mut header, size, 0o644, tar::EntryType::Regular);
    archive.append_data(&mut header, path, reader)?;
    Ok(())
}

fn append_verified_object(
    archive: &mut tar::Builder<&mut File>,
    member: &str,
    metadata: &ObjectMetadata,
    reader: &mut dyn Read,
) -> Result<(), ProtocolBundleError> {
    let mut hashing = HashingReader::new(reader, metadata.reference.algorithm());
    append_reader(archive, member, metadata.size, &mut hashing)?;
    let mut extra = [0_u8; 1];
    if hashing.read(&mut extra)? != 0 {
        return Err(ProtocolBundleError::Invalid(format!(
            "object source size changed for {}",
            metadata.reference
        )));
    }
    if hashing.bytes != metadata.size {
        return Err(ProtocolBundleError::Invalid(format!(
            "object source is truncated for {}",
            metadata.reference
        )));
    }
    verify_digest(&metadata.reference, &hashing.finish())
}

fn validate_bundle_header(header: &tar::Header) -> Result<(), ProtocolBundleError> {
    if header.entry_type() != tar::EntryType::Regular {
        return Err(ProtocolBundleError::Invalid(
            "bundle members must be regular files".to_owned(),
        ));
    }
    let mode = header.mode()?;
    let uid = header.uid()?;
    let gid = header.gid()?;
    let mtime = header.mtime()?;
    if mode != 0o644 || uid != 0 || gid != 0 || mtime != 0 {
        return Err(ProtocolBundleError::Invalid(format!(
            "non-canonical TAR header: mode={mode:o} uid={uid} gid={gid} mtime={mtime}"
        )));
    }
    Ok(())
}

fn strict_member_path(header: &tar::Header) -> Result<String, ProtocolBundleError> {
    let bytes = header.path_bytes();
    let member = std::str::from_utf8(&bytes)
        .map_err(|_| ProtocolBundleError::Invalid("member path is not UTF-8".to_owned()))?;
    if member.is_empty()
        || member.starts_with('/')
        || member
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || member.contains('\\')
    {
        return Err(ProtocolBundleError::Invalid(format!(
            "non-canonical bundle member path `{member}`"
        )));
    }
    Ok(member.to_owned())
}

fn parse_object_member(member: &str) -> Result<ContentRef, ProtocolBundleError> {
    let suffix = member
        .strip_prefix(OBJECT_PREFIX)
        .ok_or_else(|| ProtocolBundleError::Invalid(format!("invalid object member `{member}`")))?;
    let (algorithm, digest) = suffix
        .split_once('/')
        .ok_or_else(|| ProtocolBundleError::Invalid(format!("invalid object member `{member}`")))?;
    if digest.contains('/') {
        return Err(ProtocolBundleError::Invalid(format!(
            "invalid object member `{member}`"
        )));
    }
    ContentRef::parse(format!("{algorithm}:{digest}"))
        .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))
}

fn object_member(reference: &ContentRef) -> String {
    format!(
        "{OBJECT_PREFIX}{}/{}",
        reference.algorithm(),
        reference.digest()
    )
}

fn object_path(root: &Path, reference: &ContentRef) -> PathBuf {
    root.join("objects")
        .join(reference.algorithm())
        .join(reference.digest())
}

struct CopyDigest {
    bytes: u64,
    hex: String,
}

fn copy_and_hash(
    reader: &mut dyn Read,
    writer: &mut File,
    reference: &ContentRef,
) -> Result<CopyDigest, ProtocolBundleError> {
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut bytes = 0_u64;
    let mut hash = ObjectHasher::new(reference.algorithm());
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| ProtocolBundleError::Invalid("object size overflow".to_owned()))?;
    }
    Ok(CopyDigest {
        bytes,
        hex: hash.finish(),
    })
}

fn copy_exact(reader: &mut dyn Read, writer: &mut File) -> Result<u64, ProtocolBundleError> {
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| ProtocolBundleError::Invalid("member size overflow".to_owned()))?;
    }
    writer.sync_all()?;
    Ok(bytes)
}

enum ObjectHasher {
    Blake3(Box<blake3::Hasher>),
    Sha256(Sha256),
}

impl ObjectHasher {
    fn new(algorithm: &str) -> Self {
        match algorithm {
            "blake3" => Self::Blake3(Box::new(blake3::Hasher::new())),
            "sha256" => Self::Sha256(Sha256::new()),
            _ => unreachable!("ContentRef restricts hash algorithms"),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Blake3(hasher) => {
                hasher.update(bytes);
            }
            Self::Sha256(hasher) => {
                hasher.update(bytes);
            }
        }
    }

    fn finish(self) -> String {
        match self {
            Self::Blake3(hasher) => hasher.finalize().to_hex().to_string(),
            Self::Sha256(hasher) => hex::encode(hasher.finalize()),
        }
    }
}

struct HashingReader<'a> {
    inner: &'a mut dyn Read,
    hasher: Option<ObjectHasher>,
    bytes: u64,
}

impl<'a> HashingReader<'a> {
    fn new(inner: &'a mut dyn Read, algorithm: &str) -> Self {
        Self {
            inner,
            hasher: Some(ObjectHasher::new(algorithm)),
            bytes: 0,
        }
    }

    fn finish(mut self) -> String {
        self.hasher.take().expect("hasher is present").finish()
    }
}

impl Read for HashingReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.hasher
            .as_mut()
            .expect("hasher is present")
            .update(&bytes[..count]);
        self.bytes = self
            .bytes
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("object size overflow"))?;
        Ok(count)
    }
}

fn verify_digest(reference: &ContentRef, actual: &str) -> Result<(), ProtocolBundleError> {
    if actual != reference.digest() {
        return Err(ProtocolBundleError::Invalid(format!(
            "object digest mismatch for {reference}"
        )));
    }
    Ok(())
}

fn owner_only_tempfile(directory: &Path, prefix: &str) -> io::Result<NamedTempFile> {
    let file = TempBuilder::new().prefix(prefix).tempfile_in(directory)?;
    set_owner_only_file(file.path())?;
    Ok(file)
}

fn owner_only_file(path: &Path) -> io::Result<File> {
    let file = File::create(path)?;
    set_owner_only_file(path)?;
    Ok(file)
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure owner-only spool is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure owner-only spool is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use capsule_protocol::{CURRENT_SCHEMA_VERSION, ConnectorDef, StateRef};

    fn descriptor(base: ContentRef, protocol: &str) -> CapsuleDescriptor {
        CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: StateRef {
                state_type: StateTypeId::parse("ato.state.fixture-machine@1").unwrap(),
                state_ref: base,
            },
            connectors: BTreeMap::from([(
                ConnectorId::parse("terminal.main").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse(protocol).unwrap(),
                    config_ref: None,
                },
            )]),
        }
    }

    fn write_object(root: &Path, bytes: &[u8]) -> ContentRef {
        let reference = super::super::content_ref(bytes);
        let path = root.join(reference.algorithm()).join(reference.digest());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        reference
    }

    fn write_repeated_object(root: &Path, byte: u8, size: u64) -> ContentRef {
        let staging = root.join("large.staging");
        fs::create_dir_all(root).unwrap();
        let mut file = File::create(&staging).unwrap();
        let chunk = [byte; COPY_BUFFER_BYTES];
        let mut remaining = size;
        let mut hasher = blake3::Hasher::new();
        while remaining > 0 {
            let count = remaining.min(chunk.len() as u64) as usize;
            file.write_all(&chunk[..count]).unwrap();
            hasher.update(&chunk[..count]);
            remaining -= count as u64;
        }
        file.sync_all().unwrap();
        let reference =
            ContentRef::parse(format!("blake3:{}", hasher.finalize().to_hex())).unwrap();
        let path = root.join(reference.algorithm()).join(reference.digest());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::rename(staging, path).unwrap();
        reference
    }

    #[test]
    fn directory_source_streams_large_objects_into_validated_spool() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let large_size = 128 * 1024 * 1024;
        let base = write_repeated_object(&objects_root, 0, large_size);
        let payload = write_object(&objects_root, b"payload object");
        let extra = write_object(&objects_root, b"additional object");
        let descriptor = descriptor(base.clone(), "ato.io.pty@1");
        let records = vec![Ok(IoRecord {
            seq: 7,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Egress,
            kind: RecordKindId::parse("output").unwrap(),
            payload: Payload::Object(payload.clone()),
        })];
        let output = fixture.path().join("large.capsule");
        let mut policy = AllowAllPortableExportPolicy;
        let outcome = StreamingBundleWriter::write(
            &output,
            &descriptor,
            records,
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap();
        assert_eq!(outcome.summary.object_count, 3);

        let spool_root = fixture.path().join("spool");
        let bundle = StreamingBundleReader::read_into(&output, &spool_root).unwrap();
        assert_eq!(bundle.summary.object_count, 3);
        assert_eq!(bundle.summary.record_count, 1);
        assert_eq!(bundle.objects.index()[&base].size, large_size);
        assert_eq!(bundle.objects.index()[&payload].size, 14);
        assert_eq!(bundle.objects.index()[&extra].size, 17);
        let digest = super::super::content_ref_reader(bundle.objects.open(&base).unwrap()).unwrap();
        assert_eq!(digest, base);
    }

    #[test]
    fn ready_state_manifest_and_chunks_round_trip_in_sorted_order() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let manifest = write_object(&objects_root, b"ready-state-manifest");
        let mut expected = BTreeSet::from([manifest.clone()]);
        let mut state_roles = BTreeMap::new();
        for index in 0..50_u8 {
            let chunk = write_object(&objects_root, &[index; 1024]);
            expected.insert(chunk.clone());
            state_roles.insert(
                chunk,
                vec![PortableObjectRole::StateAdapterObject {
                    state_type: StateTypeId::parse("ato.state.fixture-machine@1").unwrap(),
                }],
            );
        }
        let descriptor = descriptor(manifest, "ato.io.pty@1");
        let output = fixture.path().join("ready-state.capsule");
        let mut policy = AllowAllPortableExportPolicy;
        StreamingBundleWriter::write_with_state_roles(
            &output,
            &descriptor,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &state_roles,
            &mut policy,
        )
        .unwrap();
        let spool =
            StreamingBundleReader::read_into(&output, &fixture.path().join("spool")).unwrap();
        assert_eq!(
            spool
                .objects
                .index()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected
        );

        let missing = state_roles.keys().next().unwrap().clone();
        fs::remove_file(
            objects_root
                .join(missing.algorithm())
                .join(missing.digest()),
        )
        .unwrap();
        let error = StreamingBundleWriter::write_with_state_roles(
            &fixture.path().join("missing-ready-state.capsule"),
            &descriptor,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &state_roles,
            &mut policy,
        )
        .unwrap_err();
        assert!(error.to_string().contains(missing.as_str()));
    }

    struct MultiRolePolicy {
        saw_multiple_roles: bool,
    }

    impl PortableExportPolicy for MultiRolePolicy {
        fn inspect_record(&mut self, _record: &IoRecord) -> Result<(), PortableExportError> {
            Ok(())
        }

        fn inspect_object(
            &mut self,
            _metadata: &ObjectMetadata,
            roles: &[PortableObjectRole],
            _reader: &mut dyn Read,
        ) -> Result<(), PortableExportError> {
            self.saw_multiple_roles |= roles.len() > 1;
            Ok(())
        }
    }

    #[test]
    fn one_object_can_carry_multiple_portable_roles() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let shared = write_object(&objects_root, b"shared");
        let descriptor = descriptor(shared.clone(), "ato.io.pty@1");
        let record = IoRecord {
            seq: 1,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Egress,
            kind: RecordKindId::parse("output").unwrap(),
            payload: Payload::Object(shared),
        };
        let mut policy = MultiRolePolicy {
            saw_multiple_roles: false,
        };
        StreamingBundleWriter::write(
            &fixture.path().join("roles.capsule"),
            &descriptor,
            [Ok(record)],
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap();
        assert!(policy.saw_multiple_roles);
    }

    struct BadDigestSource {
        reference: ContentRef,
    }

    impl ObjectSource for BadDigestSource {
        fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
            Ok(BTreeMap::from([(
                self.reference.clone(),
                ObjectMetadata {
                    reference: self.reference.clone(),
                    size: 5,
                },
            )]))
        }

        fn open(
            &self,
            _reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
            Ok(Box::new(Cursor::new(b"wrong".to_vec())))
        }
    }

    #[test]
    fn writer_digest_failure_leaves_no_output_or_temporary_file() {
        let fixture = tempfile::tempdir().unwrap();
        let reference = super::super::content_ref(b"right");
        let descriptor = descriptor(reference.clone(), "ato.io.pty@1");
        let output = fixture.path().join("output.capsule");
        let mut policy = AllowAllPortableExportPolicy;
        let error = StreamingBundleWriter::write(
            &output,
            &descriptor,
            std::iter::empty(),
            &BadDigestSource { reference },
            &mut policy,
        )
        .unwrap_err();
        assert!(error.to_string().contains("digest mismatch"));
        assert!(!output.exists());
        let leftovers = fs::read_dir(fixture.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "temporary files remain: {leftovers:?}"
        );
    }

    #[test]
    fn pty_policy_rejects_unknown_connectors_and_object_backed_credentials() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let base = write_object(&objects_root, b"base");
        let unknown = descriptor(base.clone(), "com.example.io.foo@1");
        let output = fixture.path().join("unknown.capsule");
        let mut policy = PtyPortableExportPolicy::new(&unknown);
        let error = StreamingBundleWriter::write(
            &output,
            &unknown,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap_err();
        assert!(error.to_string().contains("policy is unavailable"));
        assert!(!output.exists());

        let secret = b"OPENAI_API_KEY=sk-proj-ABCDEFGHIJ1234567890abcdef";
        let payload = write_object(&objects_root, secret);
        let known = descriptor(base, "ato.io.pty@1");
        let record = IoRecord {
            seq: 1,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Object(payload),
        };
        let mut policy = PtyPortableExportPolicy::new(&known);
        let error = StreamingBundleWriter::write(
            &fixture.path().join("secret.capsule"),
            &known,
            [Ok(record)],
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("credential material"));
        assert!(!message.contains("ABCDEFGHIJ1234567890abcdef"));
    }

    #[test]
    fn strict_policy_rechecks_workspace_state_and_rejects_unclassified_objects() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("safe.txt"), b"safe").unwrap();
        fs::write(workspace.join(".env"), b"TOKEN=local-only-value").unwrap();
        fs::write(
            workspace.join("leak.txt"),
            b"OPENAI_API_KEY=sk-proj-ABCDEFGHIJ1234567890abcdef",
        )
        .unwrap();
        let (state, local_checkpoint) =
            super::super::capture_local_workspace_checkpoint(&workspace).unwrap();
        let objects_root = fixture.path().join("objects");
        let checkpoint_ref = write_object(&objects_root, &local_checkpoint);
        assert_eq!(checkpoint_ref, state.state_ref);
        let descriptor = CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: state,
            connectors: BTreeMap::from([(
                ConnectorId::parse("terminal.main").unwrap(),
                ConnectorDef {
                    protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
                    config_ref: None,
                },
            )]),
        };
        let output = fixture.path().join("local-checkpoint.capsule");
        let mut policy = StrictPortableExportPolicy::new(&descriptor);
        let error = StreamingBundleWriter::write(
            &output,
            &descriptor,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("credential material") || message.contains("non-portable path"));
        assert!(!message.contains("ABCDEFGHIJ1234567890abcdef"));
        assert!(!output.exists());

        fs::remove_file(workspace.join("leak.txt")).unwrap();
        fs::remove_file(workspace.join(".env")).unwrap();
        let (safe_state, safe_bytes) = super::super::capture_workspace_state(&workspace).unwrap();
        let safe_root = fixture.path().join("safe-objects");
        write_object(&safe_root, &safe_bytes);
        let unclassified = write_object(
            &safe_root,
            b"OPENAI_API_KEY=sk-proj-ZYXWVUTSRQ9876543210abcdef",
        );
        let safe_descriptor = CapsuleDescriptor {
            base_state: safe_state,
            ..descriptor
        };
        let mut policy = StrictPortableExportPolicy::new(&safe_descriptor);
        let error = StreamingBundleWriter::write(
            &fixture.path().join("unclassified.capsule"),
            &safe_descriptor,
            std::iter::empty(),
            &DirectoryObjectSource::new(&safe_root),
            &mut policy,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ProtocolBundleError::PortableExport(PortableExportError::UnclassifiedObject(reference))
                if reference == unclassified
        ));
    }

    #[test]
    fn strict_policy_rejects_unknown_state_and_invalid_pty_direction() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let base = write_object(&objects_root, b"base");
        let unknown = descriptor(base.clone(), "ato.io.pty@1");
        let mut policy = StrictPortableExportPolicy::new(&unknown);
        let error = StreamingBundleWriter::write(
            &fixture.path().join("unknown-state.capsule"),
            &unknown,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap_err();
        assert!(error.to_string().contains("State type"));

        let bad_record = IoRecord {
            seq: 1,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: ConnectorId::parse("terminal.main").unwrap(),
            direction: Direction::Egress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Inline(b"safe".to_vec()),
        };
        let mut policy = PtyPortableExportPolicy::new(&unknown);
        let error = StreamingBundleWriter::write(
            &fixture.path().join("bad-direction.capsule"),
            &unknown,
            [Ok(bad_record)],
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap_err();
        assert!(error.to_string().contains("kind/direction"));
    }

    fn write_raw_bundle(path: &Path, members: &[(&str, &[u8], tar::EntryType)]) {
        let mut archive = tar::Builder::new(File::create(path).unwrap());
        for (member, bytes, kind) in members {
            let mut header = tar::Header::new_gnu();
            normalize_header(&mut header, bytes.len() as u64, 0o644, *kind);
            archive
                .append_data(&mut header, member, Cursor::new(*bytes))
                .unwrap();
        }
        archive.finish().unwrap();
    }

    fn write_bundle_with_raw_first_path(path: &Path, raw_path: &[u8]) {
        let mut archive = tar::Builder::new(File::create(path).unwrap());
        let mut header = tar::Header::new_gnu();
        normalize_header(&mut header, 0, 0o644, tar::EntryType::Regular);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..raw_path.len()].copy_from_slice(raw_path);
        header.set_cksum();
        archive.append(&header, std::io::empty()).unwrap();
        archive.finish().unwrap();
    }

    #[test]
    fn reader_rejects_wrong_order_duplicates_unknown_members_and_links() {
        let fixture = tempfile::tempdir().unwrap();
        let state = b"state";
        let state_ref = super::super::content_ref(state);
        let descriptor_bytes =
            encode_descriptor(&descriptor(state_ref.clone(), "ato.io.pty@1")).unwrap();
        let object_member = object_member(&state_ref);
        let cases: Vec<Vec<(&str, &[u8], tar::EntryType)>> = vec![
            vec![
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (object_member.as_str(), state, tar::EntryType::Regular),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
                ("unknown/member", b"x", tar::EntryType::Regular),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Symlink,
                ),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
            ],
            vec![
                ("pax", b"20 path=ignored\n", tar::EntryType::XHeader),
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
            ],
        ];
        for (index, members) in cases.iter().enumerate() {
            let path = fixture.path().join(format!("malicious-{index}.capsule"));
            write_raw_bundle(&path, members);
            let spool_root = fixture.path().join(format!("spool-{index}"));
            assert!(StreamingBundleReader::read_into(&path, &spool_root).is_err());
            assert_eq!(
                fs::read_dir(&spool_root).unwrap().count(),
                0,
                "invalid bundle left spool data for case {index}"
            );
        }

        for (index, raw_path) in [
            b"../descriptor".as_slice(),
            b"/absolute".as_slice(),
            &[0xff_u8][..],
        ]
        .into_iter()
        .enumerate()
        {
            let path = fixture.path().join(format!("path-{index}.capsule"));
            write_bundle_with_raw_first_path(&path, raw_path);
            assert!(
                StreamingBundleReader::read_into(
                    &path,
                    &fixture.path().join(format!("path-spool-{index}"))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn reader_rejects_unsorted_duplicate_and_bad_digest_objects() {
        let fixture = tempfile::tempdir().unwrap();
        let first_bytes = b"first";
        let second_bytes = b"second";
        let first = super::super::content_ref(first_bytes);
        let second = super::super::content_ref(second_bytes);
        let (low, low_bytes, high, high_bytes) = if first < second {
            (
                first,
                first_bytes.as_slice(),
                second,
                second_bytes.as_slice(),
            )
        } else {
            (
                second,
                second_bytes.as_slice(),
                first,
                first_bytes.as_slice(),
            )
        };
        let descriptor_bytes = encode_descriptor(&descriptor(low.clone(), "ato.io.pty@1")).unwrap();
        let low_member = object_member(&low);
        let high_member = object_member(&high);
        let cases = [
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    descriptor_bytes.as_slice(),
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"".as_slice(), tar::EntryType::Regular),
                (high_member.as_str(), high_bytes, tar::EntryType::Regular),
                (low_member.as_str(), low_bytes, tar::EntryType::Regular),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    descriptor_bytes.as_slice(),
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"".as_slice(), tar::EntryType::Regular),
                (low_member.as_str(), low_bytes, tar::EntryType::Regular),
                (low_member.as_str(), low_bytes, tar::EntryType::Regular),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    descriptor_bytes.as_slice(),
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"".as_slice(), tar::EntryType::Regular),
                (
                    low_member.as_str(),
                    b"wrong".as_slice(),
                    tar::EntryType::Regular,
                ),
            ],
            vec![
                (
                    DESCRIPTOR_MEMBER,
                    descriptor_bytes.as_slice(),
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"".as_slice(), tar::EntryType::Regular),
                (
                    "objects/md5/aaaaaaaa",
                    b"bad".as_slice(),
                    tar::EntryType::Regular,
                ),
            ],
        ];
        for (index, members) in cases.iter().enumerate() {
            let path = fixture.path().join(format!("objects-{index}.capsule"));
            write_raw_bundle(&path, members);
            assert!(
                StreamingBundleReader::read_into(
                    &path,
                    &fixture.path().join(format!("objects-spool-{index}"))
                )
                .is_err()
            );
        }
    }

    #[test]
    fn reader_rejects_oversized_physical_bundle_and_truncated_object() {
        let fixture = tempfile::tempdir().unwrap();
        let oversized = fixture.path().join("oversized.capsule");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_ARCHIVE_BYTES + 1)
            .unwrap();
        assert!(
            StreamingBundleReader::read_into(&oversized, &fixture.path().join("oversized-spool"))
                .is_err()
        );

        let object = vec![7_u8; 4096];
        let reference = super::super::content_ref(&object);
        let descriptor_bytes =
            encode_descriptor(&descriptor(reference.clone(), "ato.io.pty@1")).unwrap();
        let member = object_member(&reference);
        let truncated = fixture.path().join("truncated.capsule");
        write_raw_bundle(
            &truncated,
            &[
                (
                    DESCRIPTOR_MEMBER,
                    &descriptor_bytes,
                    tar::EntryType::Regular,
                ),
                (RECORDS_MEMBER, b"", tar::EntryType::Regular),
                (&member, &object, tar::EntryType::Regular),
            ],
        );
        let length = fs::metadata(&truncated).unwrap().len();
        File::options()
            .write(true)
            .open(&truncated)
            .unwrap()
            .set_len(length - 1024 - 2048)
            .unwrap();
        assert!(
            StreamingBundleReader::read_into(&truncated, &fixture.path().join("truncated-spool"))
                .is_err()
        );
    }

    #[test]
    fn reader_rejects_nonzero_data_after_tar_end_blocks() {
        let fixture = tempfile::tempdir().unwrap();
        let objects_root = fixture.path().join("objects");
        let base = write_object(&objects_root, b"base");
        let descriptor = descriptor(base, "ato.io.pty@1");
        let path = fixture.path().join("trailing.capsule");
        let mut policy = AllowAllPortableExportPolicy;
        StreamingBundleWriter::write(
            &path,
            &descriptor,
            std::iter::empty(),
            &DirectoryObjectSource::new(&objects_root),
            &mut policy,
        )
        .unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"second-tar-or-junk").unwrap();
        file.sync_all().unwrap();
        assert!(
            StreamingBundleReader::read_into(&path, &fixture.path().join("trailing-spool"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_object_source_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("objects");
        let reference = super::super::content_ref(b"target");
        let target = fixture.path().join("target");
        fs::write(&target, b"target").unwrap();
        let link = root.join(reference.algorithm()).join(reference.digest());
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&target, &link).unwrap();
        let source = DirectoryObjectSource::new(&root);
        assert!(source.index().is_err());
        assert!(source.open(&reference).is_err());
    }
}
