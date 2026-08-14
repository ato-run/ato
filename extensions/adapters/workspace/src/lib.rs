//! Observable and replayable mutations crossing a workspace boundary.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, WorkspaceCapturePolicy,
};
use ato_computation::ContentRef;
use ato_objects::{ObjectResolver, ObjectStore, RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const WORKSPACE_ADAPTER_ID: &str = "ato.workspace@1";
pub const WORKSPACE_PROTOCOL_ID: &str = "ato.workspace@1";
const MAX_WORKSPACE_OBJECT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceMutation {
    Put { path: String, content: String },
    Delete { path: String },
    Rename { from: String, to: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    pub files: BTreeMap<String, String>,
}

pub fn capture_workspace(
    root: &Path,
    objects: &dyn ObjectStore,
) -> Result<ContentRef, WorkspaceError> {
    capture_workspace_with_policy(root, objects, &WorkspaceCapturePolicy::secure_default())
}

pub fn capture_workspace_with_policy(
    root: &Path,
    objects: &dyn ObjectStore,
    policy: &WorkspaceCapturePolicy,
) -> Result<ContentRef, WorkspaceError> {
    let mut files = BTreeMap::new();
    capture_directory(root, root, objects, policy, &mut files)?;
    Ok(objects.put(&serde_jcs::to_vec(&WorkspaceSnapshot { files })?)?)
}

pub fn restore_workspace(
    snapshot: &ContentRef,
    destination: &Path,
    objects: &dyn ObjectResolver,
) -> Result<(), WorkspaceError> {
    let metadata = objects.metadata(snapshot)?;
    let bytes = read_exact_object(objects, snapshot, metadata.size, MAX_WORKSPACE_OBJECT_BYTES)?;
    let snapshot: WorkspaceSnapshot = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&snapshot)? != bytes {
        return Err(WorkspaceError::NonCanonical);
    }
    fs::create_dir_all(destination)?;
    for (relative, content) in snapshot.files {
        let path = checked_path(destination, &relative)?;
        let reference = ContentRef::parse(content)
            .map_err(|error| WorkspaceError::InvalidReference(error.to_string()))?;
        let metadata = objects.metadata(&reference)?;
        let bytes = read_exact_object(
            objects,
            &reference,
            metadata.size,
            MAX_WORKSPACE_OBJECT_BYTES,
        )?;
        fs::create_dir_all(path.parent().ok_or(WorkspaceError::EscapesBoundary)?)?;
        fs::write(path, bytes)?;
    }
    Ok(())
}

pub fn encode_mutation(mutation: &WorkspaceMutation) -> Result<Vec<u8>, WorkspaceError> {
    Ok(serde_jcs::to_vec(mutation)?)
}

pub fn decode_mutation(bytes: &[u8]) -> Result<WorkspaceMutation, WorkspaceError> {
    let mutation = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&mutation)? != bytes {
        return Err(WorkspaceError::NonCanonical);
    }
    Ok(mutation)
}

#[derive(Default)]
pub struct WorkspaceAdapter;

impl AdapterFactory for WorkspaceAdapter {
    fn id(&self) -> &str {
        WORKSPACE_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
        }
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        _context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        Ok(Box::new(WorkspaceSession {
            instance_id: instance.instance_id.clone(),
        }))
    }
}

struct WorkspaceSession {
    instance_id: String,
}

impl AttachedAdapter for WorkspaceSession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        WORKSPACE_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&WorkspaceAdapter)
    }

    fn apply(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes = read_exact_object(
            context.objects,
            &record.payload_ref,
            metadata.size,
            MAX_WORKSPACE_OBJECT_BYTES,
        )?;
        let mutation =
            decode_mutation(&bytes).map_err(|error| AdapterError::Operation(error.to_string()))?;
        apply_mutation(context.workspace, &mutation, context.objects)
            .map_err(|error| AdapterError::Operation(error.to_string()))
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes = read_exact_object(
            context.objects,
            &record.payload_ref,
            metadata.size,
            MAX_WORKSPACE_OBJECT_BYTES,
        )?;
        decode_mutation(&bytes)
            .map(|_| ())
            .map_err(|error| AdapterError::Operation(error.to_string()))
    }
}

pub fn apply_mutation(
    root: &Path,
    mutation: &WorkspaceMutation,
    objects: &dyn ObjectResolver,
) -> Result<(), WorkspaceError> {
    match mutation {
        WorkspaceMutation::Put { path, content } => {
            let path = checked_path(root, path)?;
            let reference = ContentRef::parse(content.clone())
                .map_err(|error| WorkspaceError::InvalidReference(error.to_string()))?;
            let metadata = objects.metadata(&reference)?;
            let bytes = read_exact_object(
                objects,
                &reference,
                metadata.size,
                MAX_WORKSPACE_OBJECT_BYTES,
            )?;
            fs::create_dir_all(path.parent().ok_or(WorkspaceError::EscapesBoundary)?)?;
            fs::write(path, bytes)?;
        }
        WorkspaceMutation::Delete { path } => {
            let path = checked_path(root, path)?;
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        WorkspaceMutation::Rename { from, to } => {
            let from = checked_path(root, from)?;
            let to = checked_path(root, to)?;
            fs::create_dir_all(to.parent().ok_or(WorkspaceError::EscapesBoundary)?)?;
            fs::rename(from, to)?;
        }
    }
    Ok(())
}

fn capture_directory(
    root: &Path,
    directory: &Path,
    objects: &dyn ObjectStore,
    policy: &WorkspaceCapturePolicy,
    files: &mut BTreeMap<String, String>,
) -> Result<(), WorkspaceError> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(WorkspaceError::Symlink(path));
        }
        if file_type.is_dir() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| WorkspaceError::EscapesBoundary)?;
            if policy.descends_into(relative) {
                capture_directory(root, &path, objects, policy, files)?;
            }
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| WorkspaceError::EscapesBoundary)?;
            if !policy.captures(relative) {
                continue;
            }
            let relative = relative
                .to_str()
                .ok_or(WorkspaceError::NonUtf8Path)?
                .replace(std::path::MAIN_SEPARATOR, "/");
            files.insert(relative, objects.put(&fs::read(path)?)?.to_string());
        }
    }
    Ok(())
}

fn checked_path(root: &Path, relative: &str) -> Result<PathBuf, WorkspaceError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::EscapesBoundary);
    }
    Ok(root.join(relative))
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("workspace JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workspace object is not canonical JCS")]
    NonCanonical,
    #[error("workspace path escapes the captured boundary")]
    EscapesBoundary,
    #[error("workspace paths must be UTF-8")]
    NonUtf8Path,
    #[error("workspace capture rejects symlink `{0}`")]
    Symlink(PathBuf),
    #[error("invalid workspace content reference: {0}")]
    InvalidReference(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_objects::MemoryObjectStore;

    #[test]
    fn snapshot_roundtrip_and_mutation_stay_inside_boundary() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("state.txt"), "A").unwrap();
        let objects = MemoryObjectStore::default();
        let snapshot = capture_workspace(source.path(), &objects).unwrap();
        restore_workspace(&snapshot, destination.path(), &objects).unwrap();
        assert_eq!(
            fs::read_to_string(destination.path().join("state.txt")).unwrap(),
            "A"
        );
        assert!(matches!(
            apply_mutation(
                destination.path(),
                &WorkspaceMutation::Delete {
                    path: "../escape".to_owned()
                },
                &objects,
            ),
            Err(WorkspaceError::EscapesBoundary)
        ));
    }
}
