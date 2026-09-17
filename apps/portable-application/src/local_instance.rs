use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ato_computation::ContentRef;
use ato_formation::authoring::STATE_FILESYSTEM_PROTOCOL;
use ato_objects::{
    CapsuleBundleDocument, PORTABLE_APPLICATION_BUNDLE_VERSION, PortableApplicationBundle,
    PortableDependencyProfile, decode_capsule_bundle_document,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::instance_snapshot::{
    BROWSER_INSTANCE_STATE_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotResourceV1,
    InstanceSnapshotV1, attach_instance_snapshot, capture_filesystem_state,
    capture_snapshot_resource_assets, decode_browser_state, encode_browser_state,
    rebind_snapshot_resource_assets, restore_filesystem_state, validate_snapshot,
};
use crate::portability_export::repack_portable_dependencies;
use crate::{bundle_sha256, validate_bundle_for_derivation};

pub const LOCAL_APPLICATION_SCHEMA: &str = "ato.local-application/1";
pub const LOCAL_INSTANCE_SCHEMA: &str = "ato.local-instance/1";
pub const LOCAL_INSTANCE_RUN_SCHEMA: &str = "ato.local-instance-run/1";
pub const LOCAL_RESTORED_SNAPSHOT_SCHEMA: &str = "ato.local-restored-instance-snapshot/1";

static LOCAL_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum LocalInstanceError {
    #[error("portable bundle is invalid: {0}")]
    Bundle(String),
    #[error("local import accepts only portable application bundles")]
    NotPortableApplication,
    #[error(
        "portable Capsule declares {count} derivations; select one with --derivation <sha256:...>"
    )]
    DerivationRequired { count: usize },
    #[error("invalid local identifier `{0}`")]
    InvalidIdentifier(String),
    #[error("local instance `{0}` does not exist")]
    UnknownInstance(String),
    #[error("local instance `{instance_id}` already has Run `{run_id}` in status `{status}`")]
    ActiveRunConflict {
        instance_id: String,
        run_id: String,
        status: String,
    },
    #[error("local instance Run lease changed while updating `{0}`")]
    RunLeaseChanged(String),
    #[error("local instance data is invalid: {0}")]
    InvalidState(String),
    #[error("local instance `{instance_id}` must be stopped before saving portable data")]
    SnapshotRequiresStopped { instance_id: String },
    #[error("local instance JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("local instance I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalApplicationMetadata {
    pub schema: String,
    pub application_id: String,
    pub application_ref: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInstanceMetadata {
    pub schema: String,
    pub instance_id: String,
    pub bundle_sha256: String,
    pub contract_ref: String,
    pub application_ref: String,
    pub application_id: String,
    pub available_derivation_refs: Vec<String>,
    pub selected_derivation_ref: String,
    pub created_at: String,
    pub data_snapshot_ref: Option<String>,
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRestoredSnapshot {
    pub schema: String,
    pub snapshot_ref: String,
    pub resources: Vec<LocalRestoredResource>,
    pub assets: Vec<LocalRestoredAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRestoredResource {
    pub slot: String,
    pub protocol: String,
    pub content_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_ref: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRestoredAsset {
    pub alias: String,
    pub asset_id: String,
    pub content_ref: String,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSurfaceSnapshot {
    pub snapshot_ref: String,
    pub local_storage: BTreeMap<String, String>,
    pub assets: Vec<LocalSurfaceAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSurfaceAsset {
    pub asset_id: String,
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalInstanceRunStatus {
    Starting,
    Active,
}

impl LocalInstanceRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Active => "active",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalInstanceRun {
    pub schema: String,
    pub run_id: String,
    pub instance_id: String,
    pub token: String,
    pub status: LocalInstanceRunStatus,
    pub pid: Option<u32>,
    pub process_start_time: Option<String>,
    pub process_group: Option<u32>,
    pub boot_session: Option<String>,
    pub url: Option<String>,
    pub receipt_path: Option<String>,
    pub created_at: String,
}

pub struct LocalRunActivation<'a> {
    pub pid: u32,
    pub process_start_time: String,
    pub process_group: u32,
    pub boot_session: String,
    pub url: String,
    pub receipt: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct LocalApplicationStore {
    root: PathBuf,
}

impl LocalApplicationStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, LocalInstanceError> {
        let root = root.into();
        create_dir_all(&root.join("apps"))?;
        create_dir_all(&root.join("instances"))?;
        Ok(Self { root })
    }

    pub fn import(
        &self,
        bundle_bytes: &[u8],
        selected_derivation: Option<&str>,
    ) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        let bundle = match decode_capsule_bundle_document(bundle_bytes)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?
        {
            CapsuleBundleDocument::PortableApplicationV3(bundle)
            | CapsuleBundleDocument::PortableApplicationV4(bundle) => bundle,
            CapsuleBundleDocument::ComputationV2(_) => {
                return Err(LocalInstanceError::NotPortableApplication);
            }
        };
        let selected_derivation = match selected_derivation {
            Some(reference) => reference,
            None if bundle.index.derivations.len() == 1 => &bundle.index.derivations[0],
            None => {
                return Err(LocalInstanceError::DerivationRequired {
                    count: bundle.index.derivations.len(),
                });
            }
        };
        let validated = validate_bundle_for_derivation(&bundle, selected_derivation)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let application_id = application_id(&validated.application_ref.to_string())?;
        let created_at = observed_at();
        let application = LocalApplicationMetadata {
            schema: LOCAL_APPLICATION_SCHEMA.to_owned(),
            application_id: application_id.clone(),
            application_ref: validated.application_ref.to_string(),
            created_at: created_at.clone(),
        };
        let application_root = self.application_root(&application_id)?;
        create_dir_all(&application_root.join("bundles"))?;
        ensure_application(&application_root.join("application.json"), &application)?;

        let transport_sha256 = bundle_sha256(bundle_bytes);
        let bundle_path = application_root
            .join("bundles")
            .join(format!("{}.capsule", digest_hex(&transport_sha256)?));
        create_or_verify(&bundle_path, bundle_bytes)?;

        let instance_id = new_id("linst");
        let instance = LocalInstanceMetadata {
            schema: LOCAL_INSTANCE_SCHEMA.to_owned(),
            instance_id: instance_id.clone(),
            bundle_sha256: transport_sha256,
            contract_ref: validated.contract_ref.to_string(),
            application_ref: validated.application_ref.to_string(),
            application_id,
            available_derivation_refs: bundle.index.derivations.clone(),
            selected_derivation_ref: validated.derivation_ref.to_string(),
            created_at,
            data_snapshot_ref: validated
                .instance_snapshot_ref
                .as_ref()
                .map(ToString::to_string),
            bindings: BTreeMap::new(),
        };
        let instance_root = self.instance_root(&instance_id)?;
        create_dir(&instance_root)?;
        let prepared = (|| {
            create_dir_all(&instance_root.join("runs"))?;
            create_dir_all(&instance_root.join("snapshots"))?;
            let restored = instance
                .data_snapshot_ref
                .as_deref()
                .map(|snapshot_ref| self.materialize_snapshot(&instance_id, &bundle, snapshot_ref))
                .transpose()?;
            self.prepare_filesystem_state(&instance_id, &validated, restored.as_ref())?;
            create_canonical(&instance_root.join("instance.json"), &instance)
        })();
        if let Err(error) = prepared {
            let _ = fs::remove_dir_all(&instance_root);
            return Err(error);
        }
        Ok(instance)
    }

    pub fn instance(&self, instance_id: &str) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        let path = self.instance_root(instance_id)?.join("instance.json");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LocalInstanceError::UnknownInstance(instance_id.to_owned()));
            }
            Err(source) => return Err(LocalInstanceError::Io { path, source }),
        };
        let instance: LocalInstanceMetadata = serde_json::from_slice(&bytes)?;
        if instance.schema != LOCAL_INSTANCE_SCHEMA || instance.instance_id != instance_id {
            return Err(LocalInstanceError::InvalidState(format!(
                "instance metadata does not match `{instance_id}`"
            )));
        }
        if serde_jcs::to_vec(&instance)? != bytes {
            return Err(LocalInstanceError::InvalidState(format!(
                "instance metadata for `{instance_id}` is not canonical"
            )));
        }
        Ok(instance)
    }

    pub fn bundle_bytes(
        &self,
        instance: &LocalInstanceMetadata,
    ) -> Result<Vec<u8>, LocalInstanceError> {
        let path = self
            .application_root(&instance.application_id)?
            .join("bundles")
            .join(format!("{}.capsule", digest_hex(&instance.bundle_sha256)?));
        let bytes = read(&path)?;
        let actual = bundle_sha256(&bytes);
        if actual != instance.bundle_sha256 {
            return Err(LocalInstanceError::InvalidState(format!(
                "stored bundle digest mismatch: expected {}, got {actual}",
                instance.bundle_sha256
            )));
        }
        Ok(bytes)
    }

    pub fn restored_snapshot(
        &self,
        instance_id: &str,
    ) -> Result<Option<LocalRestoredSnapshot>, LocalInstanceError> {
        let instance = self.instance(instance_id)?;
        let Some(reference) = instance.data_snapshot_ref.as_deref() else {
            return Ok(None);
        };
        let root = self.snapshot_root(instance_id, reference)?;
        let restored = read_restored_snapshot(&root)?;
        if restored.snapshot_ref != reference {
            return Err(LocalInstanceError::InvalidState(format!(
                "restored snapshot does not match Instance `{instance_id}`"
            )));
        }
        verify_restored_snapshot(&root, &restored)?;
        Ok(Some(restored))
    }

    pub fn local_surface_snapshot(
        &self,
        instance_id: &str,
    ) -> Result<Option<LocalSurfaceSnapshot>, LocalInstanceError> {
        let instance = self.instance(instance_id)?;
        let Some(snapshot_ref) = instance.data_snapshot_ref.as_deref() else {
            return Ok(None);
        };
        let restored = self.restored_snapshot(instance_id)?.ok_or_else(|| {
            LocalInstanceError::InvalidState("restored snapshot is missing".into())
        })?;
        let root = self.snapshot_root(instance_id, snapshot_ref)?;
        let browser_resources = restored
            .resources
            .iter()
            .filter(|resource| resource.protocol == BROWSER_INSTANCE_STATE_PROTOCOL)
            .collect::<Vec<_>>();
        if browser_resources.len() > 1 {
            return Err(LocalInstanceError::InvalidState(
                "local v0 supports one browser-state resource".to_owned(),
            ));
        }
        let local_storage = match browser_resources.first() {
            Some(resource) => decode_browser_state(&read(&root.join(&resource.path))?)
                .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?,
            None => BTreeMap::new(),
        };
        let assets = restored
            .assets
            .iter()
            .map(|asset| {
                Ok(LocalSurfaceAsset {
                    asset_id: asset.asset_id.clone(),
                    filename: asset.filename.clone(),
                    content_type: asset.content_type.clone(),
                    bytes: read(&root.join(&asset.path))?,
                })
            })
            .collect::<Result<Vec<_>, LocalInstanceError>>()?;
        Ok(Some(LocalSurfaceSnapshot {
            snapshot_ref: snapshot_ref.to_owned(),
            local_storage,
            assets,
        }))
    }

    pub fn filesystem_state_paths(
        &self,
        instance_id: &str,
    ) -> Result<BTreeMap<String, PathBuf>, LocalInstanceError> {
        let instance = self.instance(instance_id)?;
        let bundle_bytes = self.bundle_bytes(&instance)?;
        let bundle = portable_bundle(&bundle_bytes)?;
        let route = validate_bundle_for_derivation(&bundle, &instance.selected_derivation_ref)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        route
            .derivation
            .state
            .iter()
            .map(|state| {
                let path = self
                    .instance_root(instance_id)?
                    .join("state")
                    .join(&state.id);
                let metadata =
                    fs::symlink_metadata(&path).map_err(|source| LocalInstanceError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if !metadata.file_type().is_dir() {
                    return Err(LocalInstanceError::InvalidState(format!(
                        "filesystem state `{}` is not a directory",
                        state.id
                    )));
                }
                Ok((state.id.clone(), path))
            })
            .collect()
    }

    pub fn save_snapshot(
        &self,
        instance_id: &str,
        snapshot: InstanceSnapshotV1,
        content: &BTreeMap<String, Vec<u8>>,
    ) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        self.persist_snapshot(instance_id, snapshot, content, None)
    }

    pub fn save_browser_state_for_run(
        &self,
        instance_id: &str,
        run_token: &str,
        local_storage: &BTreeMap<String, String>,
    ) -> Result<Option<LocalInstanceMetadata>, LocalInstanceError> {
        self.ensure_snapshot_write_fence(instance_id, Some(run_token))?;
        let instance = self.instance(instance_id)?;
        let Some(snapshot_ref) = instance.data_snapshot_ref.as_deref() else {
            return Ok(None);
        };
        let source_bytes = self.bundle_bytes(&instance)?;
        let source = portable_bundle(&source_bytes)?;
        let snapshot_reference = ContentRef::parse(snapshot_ref.to_owned())
            .map_err(|error| LocalInstanceError::InvalidState(error.to_string()))?;
        let snapshot_bytes = source
            .payload_bytes(&snapshot_reference)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let source_snapshot: InstanceSnapshotV1 = serde_json::from_slice(&snapshot_bytes)?;
        validate_snapshot(&source_snapshot)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        if !source_snapshot
            .resources
            .iter()
            .any(|resource| resource.protocol == BROWSER_INSTANCE_STATE_PROTOCOL)
        {
            return Ok(None);
        }
        let restored = self.restored_snapshot(instance_id)?.ok_or_else(|| {
            LocalInstanceError::InvalidState("restored snapshot is missing".into())
        })?;
        let root = self.snapshot_root(instance_id, snapshot_ref)?;
        let asset_uris = restored
            .assets
            .iter()
            .map(|asset| {
                (
                    asset.alias.clone(),
                    format!("ato-asset://{}", asset.asset_id),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let browser_bytes = encode_browser_state(local_storage)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let mut content = BTreeMap::new();
        let mut resources = Vec::with_capacity(source_snapshot.resources.len());
        for resource in &source_snapshot.resources {
            let restored_resource = restored
                .resources
                .iter()
                .find(|candidate| candidate.slot == resource.slot)
                .ok_or_else(|| {
                    LocalInstanceError::InvalidState(format!(
                        "restored resource `{}` is missing",
                        resource.slot
                    ))
                })?;
            let local_bytes = if resource.protocol == BROWSER_INSTANCE_STATE_PROTOCOL {
                browser_bytes.clone()
            } else {
                read(&root.join(&restored_resource.path))?
            };
            let portable_bytes = capture_snapshot_resource_assets(
                &source_snapshot,
                resource,
                &local_bytes,
                &asset_uris,
            )
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
            let content_ref = bundle_sha256(&portable_bytes);
            let mut resource = resource.clone();
            resource.content_ref = content_ref.clone();
            resources.push(resource);
            content.insert(content_ref, portable_bytes);
        }
        for asset in &source_snapshot.assets {
            let restored_asset = restored
                .assets
                .iter()
                .find(|candidate| candidate.alias == asset.alias)
                .ok_or_else(|| {
                    LocalInstanceError::InvalidState(format!(
                        "restored Asset `{}` is missing",
                        asset.alias
                    ))
                })?;
            let bytes = read(&root.join(&restored_asset.path))?;
            if bundle_sha256(&bytes) != asset.content_ref || bytes.len() as u64 != asset.size {
                return Err(LocalInstanceError::InvalidState(format!(
                    "restored Asset `{}` changed during the Run",
                    asset.alias
                )));
            }
            content.insert(asset.content_ref.clone(), bytes);
        }
        let snapshot = InstanceSnapshotV1 {
            schema: source_snapshot.schema,
            resources,
            assets: source_snapshot.assets,
            asset_bindings: source_snapshot.asset_bindings,
        };
        self.persist_snapshot(instance_id, snapshot, &content, Some(run_token))
            .map(Some)
    }

    pub fn save_filesystem_state_for_run(
        &self,
        instance_id: &str,
        run_token: &str,
    ) -> Result<Option<LocalInstanceMetadata>, LocalInstanceError> {
        self.ensure_snapshot_write_fence(instance_id, Some(run_token))?;
        let instance = self.instance(instance_id)?;
        let source_bytes = self.bundle_bytes(&instance)?;
        let source = portable_bundle(&source_bytes)?;
        let route = validate_bundle_for_derivation(&source, &instance.selected_derivation_ref)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let Some(state) = route.derivation.state.first() else {
            return Ok(None);
        };
        let state_path = self
            .filesystem_state_paths(instance_id)?
            .remove(&state.id)
            .ok_or_else(|| {
                LocalInstanceError::InvalidState(format!(
                    "filesystem state `{}` is missing",
                    state.id
                ))
            })?;
        let filesystem_bytes = capture_filesystem_state(&state_path)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let filesystem_ref = bundle_sha256(&filesystem_bytes);

        let mut snapshot = InstanceSnapshotV1 {
            schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
            resources: Vec::new(),
            assets: Vec::new(),
            asset_bindings: Vec::new(),
        };
        let mut content = BTreeMap::from([(filesystem_ref.clone(), filesystem_bytes)]);
        let mut replaced = false;
        if let Some(snapshot_ref) = instance.data_snapshot_ref.as_deref() {
            let snapshot_reference = ContentRef::parse(snapshot_ref.to_owned())
                .map_err(|error| LocalInstanceError::InvalidState(error.to_string()))?;
            let snapshot_bytes = source
                .payload_bytes(&snapshot_reference)
                .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
            let source_snapshot: InstanceSnapshotV1 = serde_json::from_slice(&snapshot_bytes)?;
            validate_snapshot(&source_snapshot)
                .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
            let restored = self.restored_snapshot(instance_id)?.ok_or_else(|| {
                LocalInstanceError::InvalidState("restored snapshot is missing".into())
            })?;
            let root = self.snapshot_root(instance_id, snapshot_ref)?;
            let asset_uris = restored
                .assets
                .iter()
                .map(|asset| {
                    (
                        asset.alias.clone(),
                        format!("ato-asset://{}", asset.asset_id),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            for resource in &source_snapshot.resources {
                if resource.protocol == STATE_FILESYSTEM_PROTOCOL && resource.slot == state.id {
                    let mut resource = resource.clone();
                    resource.content_ref = filesystem_ref.clone();
                    snapshot.resources.push(resource);
                    replaced = true;
                    continue;
                }
                let restored_resource = restored
                    .resources
                    .iter()
                    .find(|candidate| candidate.slot == resource.slot)
                    .ok_or_else(|| {
                        LocalInstanceError::InvalidState(format!(
                            "restored resource `{}` is missing",
                            resource.slot
                        ))
                    })?;
                let local_bytes = read(&root.join(&restored_resource.path))?;
                let portable_bytes = capture_snapshot_resource_assets(
                    &source_snapshot,
                    resource,
                    &local_bytes,
                    &asset_uris,
                )
                .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
                let content_ref = bundle_sha256(&portable_bytes);
                let mut resource = resource.clone();
                resource.content_ref = content_ref.clone();
                snapshot.resources.push(resource);
                content.insert(content_ref, portable_bytes);
            }
            for asset in &source_snapshot.assets {
                let restored_asset = restored
                    .assets
                    .iter()
                    .find(|candidate| candidate.alias == asset.alias)
                    .ok_or_else(|| {
                        LocalInstanceError::InvalidState(format!(
                            "restored Asset `{}` is missing",
                            asset.alias
                        ))
                    })?;
                let bytes = read(&root.join(&restored_asset.path))?;
                if bundle_sha256(&bytes) != asset.content_ref || bytes.len() as u64 != asset.size {
                    return Err(LocalInstanceError::InvalidState(format!(
                        "restored Asset `{}` changed during the Run",
                        asset.alias
                    )));
                }
                content.insert(asset.content_ref.clone(), bytes);
            }
            snapshot.assets = source_snapshot.assets;
            snapshot.asset_bindings = source_snapshot.asset_bindings;
        }
        if !replaced {
            snapshot.resources.push(InstanceSnapshotResourceV1 {
                slot: state.id.clone(),
                protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
                content_ref: filesystem_ref,
            });
        }
        snapshot
            .resources
            .sort_by(|left, right| left.slot.cmp(&right.slot));
        self.persist_snapshot(instance_id, snapshot, &content, Some(run_token))
            .map(Some)
    }

    fn persist_snapshot(
        &self,
        instance_id: &str,
        snapshot: InstanceSnapshotV1,
        content: &BTreeMap<String, Vec<u8>>,
        run_token: Option<&str>,
    ) -> Result<LocalInstanceMetadata, LocalInstanceError> {
        self.ensure_snapshot_write_fence(instance_id, run_token)?;
        let mut instance = self.instance(instance_id)?;
        let source_bytes = self.bundle_bytes(&instance)?;
        let source = portable_bundle(&source_bytes)?;
        let source = if source.index.version == PORTABLE_APPLICATION_BUNDLE_VERSION {
            repack_portable_dependencies(
                &source,
                PortableDependencyProfile::Cached,
                &BTreeMap::new(),
            )
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?
            .1
        } else {
            source
        };
        let (bytes, saved) = attach_instance_snapshot(&source, snapshot, content)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        if !saved
            .index
            .derivations
            .contains(&instance.selected_derivation_ref)
        {
            return Err(LocalInstanceError::InvalidState(
                "snapshot export changed the selected DerivationRef".to_owned(),
            ));
        }
        let snapshot_ref = saved.index.instance_snapshot_ref.clone().ok_or_else(|| {
            LocalInstanceError::InvalidState(
                "snapshot export omitted instance_snapshot_ref".to_owned(),
            )
        })?;
        self.materialize_snapshot(instance_id, &saved, &snapshot_ref)?;

        let transport_sha256 = bundle_sha256(&bytes);
        let bundle_path = self
            .application_root(&instance.application_id)?
            .join("bundles")
            .join(format!("{}.capsule", digest_hex(&transport_sha256)?));
        create_or_verify(&bundle_path, &bytes)?;

        instance.bundle_sha256 = transport_sha256;
        instance.contract_ref = saved.index.root_contract_ref;
        instance.available_derivation_refs = saved.index.derivations;
        instance.data_snapshot_ref = Some(snapshot_ref);
        self.ensure_snapshot_write_fence(instance_id, run_token)?;
        replace_canonical(
            &self.instance_root(instance_id)?.join("instance.json"),
            &instance,
        )?;
        Ok(instance)
    }

    fn ensure_snapshot_write_fence(
        &self,
        instance_id: &str,
        run_token: Option<&str>,
    ) -> Result<(), LocalInstanceError> {
        match (run_token, self.active_run(instance_id)?) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(LocalInstanceError::SnapshotRequiresStopped {
                instance_id: instance_id.to_owned(),
            }),
            (Some(expected), Some(active)) if active.token == expected => Ok(()),
            (Some(_), _) => Err(LocalInstanceError::RunLeaseChanged(instance_id.to_owned())),
        }
    }

    pub fn export_instance(&self, instance_id: &str) -> Result<Vec<u8>, LocalInstanceError> {
        let instance = self.instance(instance_id)?;
        self.bundle_bytes(&instance)
    }

    pub fn claim_run(&self, instance_id: &str) -> Result<LocalInstanceRun, LocalInstanceError> {
        self.instance(instance_id)?;
        let active_path = self.active_run_path(instance_id)?;
        if let Some(active) = self.active_run(instance_id)? {
            return Err(LocalInstanceError::ActiveRunConflict {
                instance_id: instance_id.to_owned(),
                run_id: active.run_id,
                status: active.status.as_str().to_owned(),
            });
        }
        let run = LocalInstanceRun {
            schema: LOCAL_INSTANCE_RUN_SCHEMA.to_owned(),
            run_id: new_id("lrun"),
            instance_id: instance_id.to_owned(),
            token: new_id("token"),
            status: LocalInstanceRunStatus::Starting,
            pid: None,
            process_start_time: None,
            process_group: None,
            boot_session: None,
            url: None,
            receipt_path: None,
            created_at: observed_at(),
        };
        create_dir_all(&self.run_root(instance_id, &run.run_id)?)?;
        create_canonical(&active_path, &run)?;
        Ok(run)
    }

    pub fn active_run(
        &self,
        instance_id: &str,
    ) -> Result<Option<LocalInstanceRun>, LocalInstanceError> {
        let path = self.active_run_path(instance_id)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(LocalInstanceError::Io { path, source }),
        };
        let run: LocalInstanceRun = serde_json::from_slice(&bytes)?;
        if run.schema != LOCAL_INSTANCE_RUN_SCHEMA || run.instance_id != instance_id {
            return Err(LocalInstanceError::InvalidState(format!(
                "active Run does not match instance `{instance_id}`"
            )));
        }
        if serde_jcs::to_vec(&run)? != bytes {
            return Err(LocalInstanceError::InvalidState(format!(
                "active Run for `{instance_id}` is not canonical"
            )));
        }
        Ok(Some(run))
    }

    pub fn activate_run(
        &self,
        starting: &LocalInstanceRun,
        activation: LocalRunActivation<'_>,
    ) -> Result<LocalInstanceRun, LocalInstanceError> {
        let current = self
            .active_run(&starting.instance_id)?
            .ok_or_else(|| LocalInstanceError::RunLeaseChanged(starting.instance_id.clone()))?;
        if current.token != starting.token
            || current.run_id != starting.run_id
            || current.status != LocalInstanceRunStatus::Starting
        {
            return Err(LocalInstanceError::RunLeaseChanged(
                starting.instance_id.clone(),
            ));
        }
        let run_root = self.run_root(&starting.instance_id, &starting.run_id)?;
        let receipt_path = run_root.join("verification-receipt.json");
        create_or_verify(&receipt_path, activation.receipt)?;
        let mut active = current;
        active.status = LocalInstanceRunStatus::Active;
        active.pid = Some(activation.pid);
        active.process_start_time = Some(activation.process_start_time);
        active.process_group = Some(activation.process_group);
        active.boot_session = Some(activation.boot_session);
        active.url = Some(activation.url);
        active.receipt_path = Some("verification-receipt.json".to_owned());
        replace_canonical(&self.active_run_path(&starting.instance_id)?, &active)?;
        Ok(active)
    }

    pub fn release_run(&self, instance_id: &str, token: &str) -> Result<(), LocalInstanceError> {
        let Some(active) = self.active_run(instance_id)? else {
            return Ok(());
        };
        if active.token != token {
            return Err(LocalInstanceError::RunLeaseChanged(instance_id.to_owned()));
        }
        let path = self.active_run_path(instance_id)?;
        fs::remove_file(&path).map_err(|source| LocalInstanceError::Io { path, source })
    }

    pub fn run_root(&self, instance_id: &str, run_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(run_id)?;
        Ok(self.instance_root(instance_id)?.join("runs").join(run_id))
    }

    pub fn stop_request_path(
        &self,
        instance_id: &str,
        run_id: &str,
    ) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.run_root(instance_id, run_id)?.join("stop.request"))
    }

    pub fn stop_ack_path(
        &self,
        instance_id: &str,
        run_id: &str,
    ) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.run_root(instance_id, run_id)?.join("stop.ack"))
    }

    fn application_root(&self, application_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(application_id)?;
        Ok(self.root.join("apps").join(application_id))
    }

    fn instance_root(&self, instance_id: &str) -> Result<PathBuf, LocalInstanceError> {
        validate_id(instance_id)?;
        Ok(self.root.join("instances").join(instance_id))
    }

    fn active_run_path(&self, instance_id: &str) -> Result<PathBuf, LocalInstanceError> {
        Ok(self.instance_root(instance_id)?.join("active-run.json"))
    }

    fn snapshot_root(
        &self,
        instance_id: &str,
        snapshot_ref: &str,
    ) -> Result<PathBuf, LocalInstanceError> {
        Ok(self
            .instance_root(instance_id)?
            .join("snapshots")
            .join(digest_hex(snapshot_ref)?))
    }

    fn prepare_filesystem_state(
        &self,
        instance_id: &str,
        route: &crate::ValidatedPortableApplication,
        restored: Option<&LocalRestoredSnapshot>,
    ) -> Result<(), LocalInstanceError> {
        if route.derivation.state.is_empty() {
            return Ok(());
        }
        let state_root = self.instance_root(instance_id)?.join("state");
        create_dir_all(&state_root)?;
        for state in &route.derivation.state {
            let path = state_root.join(&state.id);
            create_dir(&path)?;
            let Some(restored_resource) = restored.and_then(|snapshot| {
                snapshot.resources.iter().find(|resource| {
                    resource.slot == state.id && resource.protocol == STATE_FILESYSTEM_PROTOCOL
                })
            }) else {
                continue;
            };
            let snapshot = restored.ok_or_else(|| {
                LocalInstanceError::InvalidState("restored snapshot is missing".to_owned())
            })?;
            let snapshot_root = self.snapshot_root(instance_id, &snapshot.snapshot_ref)?;
            let bytes = read(&snapshot_root.join(&restored_resource.path))?;
            restore_filesystem_state(&bytes, &path)
                .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        }
        Ok(())
    }

    fn materialize_snapshot(
        &self,
        instance_id: &str,
        bundle: &PortableApplicationBundle,
        snapshot_ref: &str,
    ) -> Result<LocalRestoredSnapshot, LocalInstanceError> {
        let snapshot_ref = ContentRef::parse(snapshot_ref.to_owned())
            .map_err(|error| LocalInstanceError::InvalidState(error.to_string()))?;
        let snapshot_bytes = bundle
            .payload_bytes(&snapshot_ref)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let snapshot: InstanceSnapshotV1 = serde_json::from_slice(&snapshot_bytes)?;
        validate_snapshot(&snapshot)
            .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
        let root = self.snapshot_root(instance_id, snapshot_ref.as_str())?;
        create_dir_all(&root.join("resources"))?;
        create_dir_all(&root.join("assets"))?;

        let mut assets = Vec::with_capacity(snapshot.assets.len());
        let mut asset_uris = BTreeMap::new();
        for asset in &snapshot.assets {
            let bytes = snapshot_content(bundle, &asset.content_ref)?;
            if bytes.len() as u64 != asset.size {
                return Err(LocalInstanceError::InvalidState(format!(
                    "portable Asset `{}` size does not match its metadata",
                    asset.alias
                )));
            }
            let asset_id = local_asset_id(instance_id, &asset.alias);
            asset_uris.insert(asset.alias.clone(), format!("ato-asset://{asset_id}"));
            let path = format!("assets/{asset_id}/body");
            create_dir_all(&root.join("assets").join(&asset_id))?;
            create_or_verify(&root.join(&path), &bytes)?;
            assets.push(LocalRestoredAsset {
                alias: asset.alias.clone(),
                asset_id,
                content_ref: asset.content_ref.clone(),
                filename: asset.filename.clone(),
                content_type: asset.content_type.clone(),
                size: asset.size,
                path,
            });
        }

        let mut resources = Vec::with_capacity(snapshot.resources.len());
        for (index, resource) in snapshot.resources.iter().enumerate() {
            let source_bytes = snapshot_content(bundle, &resource.content_ref)?;
            let bytes =
                rebind_snapshot_resource_assets(&snapshot, resource, &source_bytes, &asset_uris)
                    .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?;
            let content_ref = bundle_sha256(&bytes);
            let source_content_ref =
                (content_ref != resource.content_ref).then(|| resource.content_ref.clone());
            let path = format!("resources/{index:06}.bin");
            create_or_verify(&root.join(&path), &bytes)?;
            resources.push(LocalRestoredResource {
                slot: resource.slot.clone(),
                protocol: resource.protocol.clone(),
                content_ref,
                source_content_ref,
                path,
            });
        }
        let restored = LocalRestoredSnapshot {
            schema: LOCAL_RESTORED_SNAPSHOT_SCHEMA.to_owned(),
            snapshot_ref: snapshot_ref.to_string(),
            resources,
            assets,
        };
        create_or_verify(&root.join("snapshot.json"), &serde_jcs::to_vec(&restored)?)?;
        verify_restored_snapshot(&root, &restored)?;
        Ok(restored)
    }
}

fn portable_bundle(bytes: &[u8]) -> Result<PortableApplicationBundle, LocalInstanceError> {
    match decode_capsule_bundle_document(bytes)
        .map_err(|error| LocalInstanceError::Bundle(error.to_string()))?
    {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => Ok(bundle),
        CapsuleBundleDocument::ComputationV2(_) => Err(LocalInstanceError::NotPortableApplication),
    }
}

fn snapshot_content(
    bundle: &PortableApplicationBundle,
    reference: &str,
) -> Result<Vec<u8>, LocalInstanceError> {
    let reference = ContentRef::parse(reference.to_owned())
        .map_err(|error| LocalInstanceError::InvalidState(error.to_string()))?;
    bundle
        .payload_bytes(&reference)
        .map_err(|error| LocalInstanceError::Bundle(error.to_string()))
}

fn local_asset_id(instance_id: &str, alias: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(instance_id.as_bytes());
    digest.update([0]);
    digest.update(alias.as_bytes());
    let digest = digest.finalize();
    let mut value = u128::from_be_bytes(digest[..16].try_into().expect("fixed SHA-256 prefix"));
    const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut encoded = [b'0'; 26];
    for character in encoded.iter_mut().rev() {
        *character = CROCKFORD_BASE32[(value & 31) as usize];
        value >>= 5;
    }
    format!(
        "ast_{}",
        std::str::from_utf8(&encoded).expect("Crockford alphabet is UTF-8")
    )
}

fn read_restored_snapshot(root: &Path) -> Result<LocalRestoredSnapshot, LocalInstanceError> {
    let path = root.join("snapshot.json");
    let bytes = read(&path)?;
    let restored: LocalRestoredSnapshot = serde_json::from_slice(&bytes)?;
    if restored.schema != LOCAL_RESTORED_SNAPSHOT_SCHEMA || serde_jcs::to_vec(&restored)? != bytes {
        return Err(LocalInstanceError::InvalidState(format!(
            "restored snapshot metadata is invalid at {}",
            path.display()
        )));
    }
    Ok(restored)
}

fn verify_restored_snapshot(
    root: &Path,
    restored: &LocalRestoredSnapshot,
) -> Result<(), LocalInstanceError> {
    let mut paths = BTreeMap::new();
    for resource in &restored.resources {
        verify_restored_content(root, &resource.path, &resource.content_ref, None)?;
        if resource.slot.is_empty()
            || resource.protocol.is_empty()
            || paths
                .insert(resource.path.as_str(), resource.slot.as_str())
                .is_some()
        {
            return Err(LocalInstanceError::InvalidState(
                "restored resource metadata is invalid".to_owned(),
            ));
        }
    }
    for asset in &restored.assets {
        validate_id(&asset.asset_id)?;
        verify_restored_content(root, &asset.path, &asset.content_ref, Some(asset.size))?;
        if asset.alias.is_empty()
            || paths
                .insert(asset.path.as_str(), asset.alias.as_str())
                .is_some()
        {
            return Err(LocalInstanceError::InvalidState(
                "restored Asset metadata is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn verify_restored_content(
    root: &Path,
    relative: &str,
    expected_ref: &str,
    expected_size: Option<u64>,
) -> Result<(), LocalInstanceError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(LocalInstanceError::InvalidState(format!(
            "restored snapshot path is unsafe: `{relative}`"
        )));
    }
    let bytes = read(&root.join(path))?;
    if expected_size.is_some_and(|size| size != bytes.len() as u64)
        || bundle_sha256(&bytes) != expected_ref
    {
        return Err(LocalInstanceError::InvalidState(format!(
            "restored snapshot content does not match `{expected_ref}`"
        )));
    }
    Ok(())
}

fn application_id(reference: &str) -> Result<String, LocalInstanceError> {
    let digest = digest_hex(reference)?;
    Ok(format!("lapp_{}", &digest[..32]))
}

fn digest_hex(reference: &str) -> Result<&str, LocalInstanceError> {
    let Some(digest) = reference.strip_prefix("sha256:") else {
        return Err(LocalInstanceError::InvalidState(format!(
            "expected SHA-256 reference, got `{reference}`"
        )));
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LocalInstanceError::InvalidState(format!(
            "invalid SHA-256 reference `{reference}`"
        )));
    }
    Ok(digest)
}

fn new_id(prefix: &str) -> String {
    let counter = LOCAL_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    let mut digest = Sha256::new();
    digest.update(prefix.as_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(nanos.to_le_bytes());
    digest.update(counter.to_le_bytes());
    format!("{prefix}_{:x}", digest.finalize())
}

fn observed_at() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_millis().to_string())
}

fn validate_id(value: &str) -> Result<(), LocalInstanceError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(LocalInstanceError::InvalidIdentifier(value.to_owned()));
    }
    Ok(())
}

fn create_dir(path: &Path) -> Result<(), LocalInstanceError> {
    fs::create_dir(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), LocalInstanceError> {
    fs::create_dir_all(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read(path: &Path) -> Result<Vec<u8>, LocalInstanceError> {
    fs::read(path).map_err(|source| LocalInstanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn create_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalInstanceError> {
    let bytes = serde_jcs::to_vec(value)?;
    create_new(path, &bytes)
}

fn ensure_application(
    path: &Path,
    expected: &LocalApplicationMetadata,
) -> Result<(), LocalInstanceError> {
    match fs::read(path) {
        Ok(bytes) => {
            let existing: LocalApplicationMetadata = serde_json::from_slice(&bytes)?;
            if serde_jcs::to_vec(&existing)? != bytes
                || existing.schema != LOCAL_APPLICATION_SCHEMA
                || existing.application_id != expected.application_id
                || existing.application_ref != expected.application_ref
            {
                return Err(LocalInstanceError::InvalidState(format!(
                    "existing local Application differs at {}",
                    path.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_canonical(path, expected)
        }
        Err(source) => Err(LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn create_or_verify(path: &Path, bytes: &[u8]) -> Result<(), LocalInstanceError> {
    match create_new(path, bytes) {
        Ok(()) => Ok(()),
        Err(LocalInstanceError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            let existing = read(path)?;
            if existing == bytes {
                Ok(())
            } else {
                Err(LocalInstanceError::InvalidState(format!(
                    "existing immutable file differs at {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error),
    }
}

fn create_new(path: &Path, bytes: &[u8]) -> Result<(), LocalInstanceError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|source| LocalInstanceError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn replace_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), LocalInstanceError> {
    let bytes = serde_jcs::to_vec(value)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", new_id("write")));
    create_new(&temporary, &bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            Err(LocalInstanceError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_snapshot::{
        DATA_JSON_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetBindingV1,
        InstanceSnapshotAssetLocationV1, InstanceSnapshotAssetV1, InstanceSnapshotResourceV1,
    };
    use crate::{
        OCI_CPU_MILLIS_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME,
        OCI_PIDS_LIMIT_RUNTIME, OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableDynamicBundleSpec,
        PortableExecutionSpec, PortableFilesystemStateSpec, PortableHttpRequirementSpec,
        PortableRealizationKind, build_dynamic_process_oci_bundle, build_multi_derivation_bundle,
        build_static_bundle, validate_bytes_all,
    };

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
    }

    fn multi_fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-multi-derivation")
    }

    fn stateful_fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/portable-stateful-notes")
    }

    fn stateful_fixture() -> (Vec<u8>, PortableApplicationBundle, String) {
        let environment = BTreeMap::from([
            ("APP_DB_PATH".to_owned(), "/data/notes.sqlite".to_owned()),
            ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
        ]);
        let spec = PortableDynamicBundleSpec {
            title: "Portable Notes".to_owned(),
            surface_path: "/".to_owned(),
            guest_port: 8000,
            process: PortableExecutionSpec {
                runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
                argv: vec!["python3".to_owned(), "-B".to_owned(), "app.py".to_owned()],
                cwd: ".".to_owned(),
                env: environment.clone(),
            },
            oci: PortableExecutionSpec {
                runtimes: BTreeMap::from([
                    (
                        OCI_IMAGE_RUNTIME.to_owned(),
                        "docker.io/library/python@sha256:1c44018d7eb40488f29e7c6ad4991d3200507e14dca71b94fe61011815e98155".to_owned(),
                    ),
                    (OCI_PLATFORM_RUNTIME.to_owned(), "linux/amd64".to_owned()),
                    (OCI_MEMORY_BYTES_RUNTIME.to_owned(), "268435456".to_owned()),
                    (OCI_CPU_MILLIS_RUNTIME.to_owned(), "1000".to_owned()),
                    (OCI_PIDS_LIMIT_RUNTIME.to_owned(), "128".to_owned()),
                ]),
                argv: vec![
                    "python3".to_owned(),
                    "-B".to_owned(),
                    "/app/app.py".to_owned(),
                ],
                cwd: ".".to_owned(),
                env: environment,
            },
            filesystem_state: Some(PortableFilesystemStateSpec {
                id: "data".to_owned(),
                mount: "/data".to_owned(),
            }),
            bindings: Vec::new(),
            requirements: vec![PortableHttpRequirementSpec {
                id: "notes-health".to_owned(),
                path: "/health".to_owned(),
                status: 200,
                body_digest: Some(
                    "sha256:4062edaf750fb8074e7e83e0c9028c94e32468a8b6f1614774328ef045150f93"
                        .to_owned(),
                ),
            }],
        };
        let (bytes, bundle) = build_dynamic_process_oci_bundle(&stateful_fixture_root(), &spec)
            .expect("stateful fixture should build");
        let process = validate_bytes_all(&bytes)
            .expect("stateful fixture should validate")
            .1
            .into_iter()
            .find(|route| route.realization == PortableRealizationKind::LocalProcess)
            .expect("stateful fixture should contain process route")
            .derivation_ref
            .to_string();
        (bytes, bundle, process)
    }

    fn snapshot_fixture() -> (InstanceSnapshotV1, BTreeMap<String, Vec<u8>>) {
        let saved_data = br#"{"photo":"ato-asset-alias://asset-1","todos":["one","two"]}"#.to_vec();
        let asset = b"portable-photo-bytes".to_vec();
        let saved_data_ref = bundle_sha256(&saved_data);
        let asset_ref = bundle_sha256(&asset);
        (
            InstanceSnapshotV1 {
                schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
                resources: vec![InstanceSnapshotResourceV1 {
                    slot: "main".to_owned(),
                    protocol: DATA_JSON_PROTOCOL.to_owned(),
                    content_ref: saved_data_ref.clone(),
                }],
                assets: vec![InstanceSnapshotAssetV1 {
                    alias: "asset-1".to_owned(),
                    content_ref: asset_ref.clone(),
                    filename: "photo.jpg".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    size: asset.len() as u64,
                }],
                asset_bindings: vec![InstanceSnapshotAssetBindingV1 {
                    alias: "asset-1".to_owned(),
                    resource_slot: "main".to_owned(),
                    location: InstanceSnapshotAssetLocationV1::DataJson {
                        pointer: "/photo".to_owned(),
                    },
                }],
            },
            BTreeMap::from([(saved_data_ref, saved_data), (asset_ref, asset)]),
        )
    }

    fn browser_snapshot_fixture() -> (InstanceSnapshotV1, BTreeMap<String, Vec<u8>>) {
        let asset = b"portable-photo-bytes".to_vec();
        let asset_ref = bundle_sha256(&asset);
        let state_value = serde_jcs::to_string(&serde_json::json!({
            "photo": "ato-asset-alias://asset-1",
            "todos": ["one", "two"]
        }))
        .unwrap();
        let browser_state = encode_browser_state(&BTreeMap::from([(
            "portable-todo-v1".to_owned(),
            state_value,
        )]))
        .unwrap();
        let browser_state_ref = bundle_sha256(&browser_state);
        (
            InstanceSnapshotV1 {
                schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
                resources: vec![InstanceSnapshotResourceV1 {
                    slot: "main".to_owned(),
                    protocol: BROWSER_INSTANCE_STATE_PROTOCOL.to_owned(),
                    content_ref: browser_state_ref.clone(),
                }],
                assets: vec![InstanceSnapshotAssetV1 {
                    alias: "asset-1".to_owned(),
                    content_ref: asset_ref.clone(),
                    filename: "photo.jpg".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    size: asset.len() as u64,
                }],
                asset_bindings: vec![InstanceSnapshotAssetBindingV1 {
                    alias: "asset-1".to_owned(),
                    resource_slot: "main".to_owned(),
                    location: InstanceSnapshotAssetLocationV1::BrowserLocalStorage {
                        key: "portable-todo-v1".to_owned(),
                        pointer: "/photo".to_owned(),
                        json_encoded: true,
                    },
                }],
            },
            BTreeMap::from([(browser_state_ref, browser_state), (asset_ref, asset)]),
        )
    }

    fn bundle_with_snapshot() -> (Vec<u8>, PortableApplicationBundle) {
        let (_, source) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let (_, source) = repack_portable_dependencies(
            &source,
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap();
        let (snapshot, content) = snapshot_fixture();
        attach_instance_snapshot(&source, snapshot, &content).unwrap()
    }

    fn bundle_with_browser_snapshot() -> (Vec<u8>, PortableApplicationBundle) {
        let (_, source) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let (_, source) = repack_portable_dependencies(
            &source,
            PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap();
        let (snapshot, content) = browser_snapshot_fixture();
        attach_instance_snapshot(&source, snapshot, &content).unwrap()
    }

    #[test]
    fn imports_independent_instances_while_reusing_immutable_application_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, bundle) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();

        let first = store.import(&bytes, None).unwrap();
        let second = store.import(&bytes, None).unwrap();

        assert_ne!(first.instance_id, second.instance_id);
        assert_eq!(first.application_id, second.application_id);
        assert_eq!(first.bundle_sha256, second.bundle_sha256);
        assert_eq!(first.contract_ref, bundle.index.root_contract_ref);
        assert_eq!(store.bundle_bytes(&first).unwrap(), bytes);
    }

    #[test]
    fn claims_only_one_active_run_and_fences_release_by_token() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let run = store.claim_run(&instance.instance_id).unwrap();

        assert!(store.claim_run(&instance.instance_id).is_err());
        assert!(
            store
                .release_run(&instance.instance_id, "wrong-token")
                .is_err()
        );
        store
            .release_run(&instance.instance_id, &run.token)
            .unwrap();
        assert!(store.active_run(&instance.instance_id).unwrap().is_none());
    }

    #[test]
    fn import_requires_an_explicit_route_when_multiple_derivations_are_declared() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, bundle) =
            build_multi_derivation_bundle(&multi_fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();

        assert!(matches!(
            store.import(&bytes, None),
            Err(LocalInstanceError::DerivationRequired { count: 2 })
        ));
        let instance = store
            .import(&bytes, Some(&bundle.index.derivations[0]))
            .unwrap();
        assert_eq!(
            instance.selected_derivation_ref,
            bundle.index.derivations[0]
        );
    }

    #[test]
    fn stored_bundle_tamper_is_rejected_before_a_run_can_start() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let bundle_path = store
            .application_root(&instance.application_id)
            .unwrap()
            .join("bundles")
            .join(format!(
                "{}.capsule",
                digest_hex(&instance.bundle_sha256).unwrap()
            ));

        fs::write(bundle_path, b"tampered").unwrap();

        assert!(
            store
                .bundle_bytes(&instance)
                .unwrap_err()
                .to_string()
                .contains("stored bundle digest mismatch")
        );
    }

    #[test]
    fn snapshot_import_restores_content_into_independent_asset_namespaces() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = bundle_with_snapshot();
        let store = LocalApplicationStore::open(root.path()).unwrap();

        let first = store.import(&bytes, None).unwrap();
        let second = store.import(&bytes, None).unwrap();
        let first_snapshot = store
            .restored_snapshot(&first.instance_id)
            .unwrap()
            .unwrap();
        let second_snapshot = store
            .restored_snapshot(&second.instance_id)
            .unwrap()
            .unwrap();

        assert_ne!(first.instance_id, second.instance_id);
        assert_eq!(first.data_snapshot_ref, second.data_snapshot_ref);
        assert_ne!(
            first_snapshot.assets[0].asset_id,
            second_snapshot.assets[0].asset_id
        );
        assert_eq!(
            first_snapshot.assets[0].content_ref,
            second_snapshot.assets[0].content_ref
        );
        let first_resource = fs::read(
            store
                .snapshot_root(
                    &first.instance_id,
                    first.data_snapshot_ref.as_deref().unwrap(),
                )
                .unwrap()
                .join(&first_snapshot.resources[0].path),
        )
        .unwrap();
        let second_resource = fs::read(
            store
                .snapshot_root(
                    &second.instance_id,
                    second.data_snapshot_ref.as_deref().unwrap(),
                )
                .unwrap()
                .join(&second_snapshot.resources[0].path),
        )
        .unwrap();
        assert!(
            String::from_utf8(first_resource)
                .unwrap()
                .contains(&format!(
                    "ato-asset://{}",
                    first_snapshot.assets[0].asset_id
                ))
        );
        assert!(
            String::from_utf8(second_resource)
                .unwrap()
                .contains(&format!(
                    "ato-asset://{}",
                    second_snapshot.assets[0].asset_id
                ))
        );
    }

    #[test]
    fn browser_snapshot_exposes_rebound_state_and_asset_bytes_to_the_local_surface() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = bundle_with_browser_snapshot();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();

        let surface = store
            .local_surface_snapshot(&instance.instance_id)
            .unwrap()
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_str(surface.local_storage.get("portable-todo-v1").unwrap()).unwrap();

        assert_eq!(surface.assets[0].bytes, b"portable-photo-bytes");
        assert_eq!(surface.assets[0].asset_id.len(), 30);
        assert_eq!(
            value["photo"],
            format!("ato-asset://{}", surface.assets[0].asset_id)
        );
    }

    #[test]
    fn run_state_save_mints_new_k_without_changing_d_and_reimports_independently() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, original) = bundle_with_browser_snapshot();
        let original_derivations = original.index.derivations;
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let imported = store.import(&bytes, None).unwrap();
        let original_surface = store
            .local_surface_snapshot(&imported.instance_id)
            .unwrap()
            .unwrap();
        let run = store.claim_run(&imported.instance_id).unwrap();
        let mut local_storage = original_surface.local_storage.clone();
        let mut state: serde_json::Value =
            serde_json::from_str(local_storage.get("portable-todo-v1").unwrap()).unwrap();
        state["todos"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::Value::String("three".to_owned()));
        local_storage.insert(
            "portable-todo-v1".to_owned(),
            serde_jcs::to_string(&state).unwrap(),
        );

        let saved = store
            .save_browser_state_for_run(&imported.instance_id, &run.token, &local_storage)
            .unwrap()
            .unwrap();
        store
            .release_run(&imported.instance_id, &run.token)
            .unwrap();
        let exported = store.export_instance(&imported.instance_id).unwrap();
        let (exported_bundle, routes) = validate_bytes_all(&exported).unwrap();
        let reimported = store.import(&exported, None).unwrap();
        let reimported_surface = store
            .local_surface_snapshot(&reimported.instance_id)
            .unwrap()
            .unwrap();
        let reimported_state: serde_json::Value = serde_json::from_str(
            reimported_surface
                .local_storage
                .get("portable-todo-v1")
                .unwrap(),
        )
        .unwrap();

        assert_ne!(saved.contract_ref, imported.contract_ref);
        assert_eq!(saved.available_derivation_refs, original_derivations);
        assert_eq!(exported_bundle.index.root_contract_ref, saved.contract_ref);
        assert!(routes.iter().all(|route| {
            route.contract_ref.to_string() == saved.contract_ref
                && route.derivation_ref.to_string() == saved.selected_derivation_ref
        }));
        assert_ne!(reimported.instance_id, imported.instance_id);
        assert_ne!(
            reimported_surface.assets[0].asset_id,
            original_surface.assets[0].asset_id
        );
        assert_eq!(
            reimported_surface.assets[0].content_type,
            original_surface.assets[0].content_type
        );
        assert_eq!(reimported_state["todos"][2], "three");
    }

    #[test]
    fn browser_state_save_rejects_a_stale_run_token() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = bundle_with_browser_snapshot();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let surface = store
            .local_surface_snapshot(&instance.instance_id)
            .unwrap()
            .unwrap();
        let run = store.claim_run(&instance.instance_id).unwrap();

        let error = store
            .save_browser_state_for_run(
                &instance.instance_id,
                "stale-token",
                &surface.local_storage,
            )
            .unwrap_err();

        assert!(matches!(error, LocalInstanceError::RunLeaseChanged(_)));
        store
            .release_run(&instance.instance_id, &run.token)
            .unwrap();
    }

    #[test]
    fn filesystem_state_save_roundtrips_through_an_independent_instance() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, original, process_ref) = stateful_fixture();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let imported = store.import(&bytes, Some(&process_ref)).unwrap();
        let state_path = store
            .filesystem_state_paths(&imported.instance_id)
            .unwrap()
            .remove("data")
            .unwrap();
        fs::write(state_path.join("notes.sqlite"), b"saved database").unwrap();
        let run = store.claim_run(&imported.instance_id).unwrap();

        let saved = store
            .save_filesystem_state_for_run(&imported.instance_id, &run.token)
            .unwrap()
            .unwrap();
        store
            .release_run(&imported.instance_id, &run.token)
            .unwrap();
        let exported = store.export_instance(&imported.instance_id).unwrap();
        let reimported = store.import(&exported, Some(&process_ref)).unwrap();
        let restored_path = store
            .filesystem_state_paths(&reimported.instance_id)
            .unwrap()
            .remove("data")
            .unwrap();

        assert_ne!(saved.contract_ref, original.index.root_contract_ref);
        assert_eq!(saved.selected_derivation_ref, process_ref);
        assert_ne!(reimported.instance_id, imported.instance_id);
        assert_eq!(
            fs::read(restored_path.join("notes.sqlite")).unwrap(),
            b"saved database"
        );
    }

    #[test]
    fn saving_snapshot_mints_new_k_and_exports_the_updated_bundle() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, original) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let original_derivations = original.index.derivations;
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let imported = store.import(&bytes, None).unwrap();
        let (snapshot, content) = snapshot_fixture();

        let saved = store
            .save_snapshot(&imported.instance_id, snapshot, &content)
            .unwrap();
        let exported = store.export_instance(&imported.instance_id).unwrap();
        let (exported_bundle, validated) = validate_bytes_all(&exported).unwrap();

        assert_ne!(saved.contract_ref, imported.contract_ref);
        assert_eq!(saved.available_derivation_refs, original_derivations);
        assert_eq!(
            saved.data_snapshot_ref,
            exported_bundle.index.instance_snapshot_ref
        );
        assert!(validated.iter().all(|route| {
            route.contract_ref.to_string() == saved.contract_ref
                && route.derivation_ref.to_string() == saved.selected_derivation_ref
        }));
    }

    #[test]
    fn active_run_blocks_snapshot_save() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = build_static_bundle(&fixture_root(), "fixture").unwrap();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let run = store.claim_run(&instance.instance_id).unwrap();
        let (snapshot, content) = snapshot_fixture();

        let result = store.save_snapshot(&instance.instance_id, snapshot, &content);

        assert!(matches!(
            result,
            Err(LocalInstanceError::SnapshotRequiresStopped { .. })
        ));
        store
            .release_run(&instance.instance_id, &run.token)
            .unwrap();
    }

    #[test]
    fn restored_asset_tamper_is_detected_before_use() {
        let root = tempfile::tempdir().unwrap();
        let (bytes, _) = bundle_with_snapshot();
        let store = LocalApplicationStore::open(root.path()).unwrap();
        let instance = store.import(&bytes, None).unwrap();
        let snapshot = store
            .restored_snapshot(&instance.instance_id)
            .unwrap()
            .unwrap();
        let snapshot_root = store
            .snapshot_root(
                &instance.instance_id,
                instance.data_snapshot_ref.as_deref().unwrap(),
            )
            .unwrap();
        fs::write(snapshot_root.join(&snapshot.assets[0].path), b"tampered").unwrap();

        let error = store.restored_snapshot(&instance.instance_id).unwrap_err();

        assert!(error.to_string().contains("does not match"));
    }
}
