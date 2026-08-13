use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::fs::File;

use capsule_core::{ComputationRef, ComputationTypeId, ContentRef as ComputationObjectRef};
use capsule_protocol::{ContentRef as ProtocolContentRef, StateTypeId};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{DurableFrontier, RecordFrontier};

const SESSION_STORE_SCHEMA_VERSION: u16 = 4;
const SESSION_SECRET_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(value: impl Into<String>) -> Result<Self, SessionStoreError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 255
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(SessionStoreError::InvalidSessionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorIdentity {
    pub generation: u64,
    pub incarnation_nonce: String,
    pub pid: u32,
    pub process_start_identity: String,
    secret_digest: String,
}

pub struct NewSupervisorIdentity {
    pub identity: SupervisorIdentity,
    secret: [u8; SESSION_SECRET_BYTES],
}

impl NewSupervisorIdentity {
    pub fn generate(generation: u64, pid: u32, process_start_identity: impl Into<String>) -> Self {
        let mut secret = [0_u8; SESSION_SECRET_BYTES];
        let mut nonce = [0_u8; 16];
        rand::thread_rng().fill_bytes(&mut secret);
        rand::thread_rng().fill_bytes(&mut nonce);
        let identity = SupervisorIdentity {
            generation,
            incarnation_nonce: hex_bytes(&nonce),
            pid,
            process_start_identity: process_start_identity.into(),
            secret_digest: blake3::hash(&secret).to_hex().to_string(),
        };
        Self { identity, secret }
    }

    pub fn secret(&self) -> &[u8; SESSION_SECRET_BYTES] {
        &self.secret
    }
}

impl SupervisorIdentity {
    pub fn authorize(
        &self,
        secret: &[u8],
        generation: u64,
        incarnation_nonce: &str,
        pid: u32,
        process_start_identity: &str,
    ) -> Result<(), ControlAuthorizationError> {
        if generation != self.generation
            || incarnation_nonce != self.incarnation_nonce
            || pid != self.pid
            || process_start_identity != self.process_start_identity
        {
            return Err(ControlAuthorizationError::StaleIncarnation);
        }
        if blake3::hash(secret).to_hex().as_str() != self.secret_digest {
            return Err(ControlAuthorizationError::InvalidSecret);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredConnectorCheckpoint {
    pub protocol_id: String,
    pub applied_at: DurableFrontier,
    pub format: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLocalCheckpoint {
    pub state_ref: String,
    pub captured_at: DurableFrontier,
    pub workspace_digest: String,
    pub resume_fidelity: String,
    #[serde(default)]
    pub connector_checkpoints: BTreeMap<String, StoredConnectorCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredReplayVerification {
    pub connector: String,
    pub protocol: String,
    pub from: RecordFrontier,
    pub through: RecordFrontier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredLegacyV1Materialization {
    WorkspacePty {
        workspace: PathBuf,
    },
    ReadyState {
        backend_id: String,
        ready_state_manifest_id: String,
        cas_root: PathBuf,
        overlay_root: PathBuf,
        vmm_pid: Option<i32>,
        vmm_process_start_identity: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLegacyV1Recovery {
    pub durable_frontier: DurableFrontier,
    pub latest_consistent_frontier: Option<DurableFrontier>,
    pub base_frontier: RecordFrontier,
    #[serde(default)]
    pub base_connector_checkpoints: BTreeMap<String, StoredConnectorCheckpoint>,
    pub active_checkpoint: Option<StoredLocalCheckpoint>,
    pub historical_replay: Option<StoredReplayVerification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredRuntimeProfile {
    LegacyV1 {
        materialization: StoredLegacyV1Materialization,
        recovery: StoredLegacyV1Recovery,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredComputationOrigin {
    Native {
        computation_type: String,
        #[serde(alias = "computation_ref")]
        object_ref: String,
    },
    #[serde(rename = "legacy_v3_state", alias = "legacy_state_io")]
    LegacyV3State {
        state_type: String,
        state_ref: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredProtocolSession {
    pub schema_version: u16,
    pub session_id: SessionId,
    pub lifecycle: String,
    #[serde(alias = "base_computation")]
    pub origin_computation: StoredComputationOrigin,
    pub runtime_profile: StoredRuntimeProfile,
    pub source_session_id: Option<SessionId>,
    pub supervisor: SupervisorIdentity,
}

pub struct NewStoredProtocolSession {
    pub session_id: SessionId,
    pub lifecycle: String,
    pub origin_computation: StoredComputationOrigin,
    pub runtime_profile: StoredRuntimeProfile,
    pub supervisor: SupervisorIdentity,
}

impl StoredProtocolSession {
    pub fn new(input: NewStoredProtocolSession) -> Self {
        Self {
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            session_id: input.session_id,
            lifecycle: input.lifecycle,
            origin_computation: input.origin_computation,
            runtime_profile: input.runtime_profile,
            source_session_id: None,
            supervisor: input.supervisor,
        }
    }

    fn validate(&self) -> Result<(), SessionStoreError> {
        if self.schema_version != SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedSchema(self.schema_version));
        }
        SessionId::parse(self.session_id.to_string())?;
        match &self.origin_computation {
            StoredComputationOrigin::Native {
                computation_type,
                object_ref,
            } => {
                ComputationTypeId::parse(computation_type)
                    .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
                ComputationObjectRef::parse(object_ref)
                    .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
            }
            StoredComputationOrigin::LegacyV3State {
                state_type,
                state_ref,
            } => {
                StateTypeId::parse(state_type)
                    .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
                ProtocolContentRef::parse(state_ref)
                    .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
            }
        }
        let (materialization, recovery) = self.legacy_v1();
        match materialization {
            StoredLegacyV1Materialization::WorkspacePty { workspace } => {
                if !workspace.is_absolute() {
                    return Err(SessionStoreError::InvalidRecord(
                        "workspace path must be absolute".to_owned(),
                    ));
                }
            }
            StoredLegacyV1Materialization::ReadyState {
                backend_id,
                ready_state_manifest_id,
                cas_root,
                overlay_root,
                vmm_pid,
                vmm_process_start_identity,
            } => {
                let manifest_id_valid = ProtocolContentRef::parse(ready_state_manifest_id).is_ok();
                if backend_id.trim().is_empty()
                    || !manifest_id_valid
                    || !cas_root.is_absolute()
                    || !overlay_root.is_absolute()
                    || vmm_pid.is_some() != vmm_process_start_identity.is_some()
                    || vmm_pid.is_some_and(|pid| pid <= 0)
                    || vmm_process_start_identity
                        .as_ref()
                        .is_some_and(|identity| identity.trim().is_empty())
                {
                    return Err(SessionStoreError::InvalidRecord(
                        "invalid Ready-State runtime profile".to_owned(),
                    ));
                }
            }
        }
        if let Some(checkpoint) = &recovery.active_checkpoint {
            ProtocolContentRef::parse(&checkpoint.state_ref)
                .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
            ProtocolContentRef::parse(&checkpoint.workspace_digest)
                .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
            if checkpoint.resume_fidelity != "filesystem_restart" {
                return Err(SessionStoreError::InvalidRecord(
                    "unsupported local checkpoint resume fidelity".to_owned(),
                ));
            }
            if recovery.latest_consistent_frontier != Some(checkpoint.captured_at) {
                return Err(SessionStoreError::InvalidRecord(
                    "active checkpoint must match latest consistent frontier".to_owned(),
                ));
            }
            for (connector, connector_checkpoint) in &checkpoint.connector_checkpoints {
                if connector.is_empty()
                    || connector_checkpoint.protocol_id.is_empty()
                    || connector_checkpoint.format.is_empty()
                {
                    return Err(SessionStoreError::InvalidRecord(
                        "Connector checkpoint identity is empty".to_owned(),
                    ));
                }
                if connector_checkpoint.applied_at != checkpoint.captured_at {
                    return Err(SessionStoreError::InvalidRecord(
                        "Connector checkpoint frontier does not match local checkpoint".to_owned(),
                    ));
                }
            }
        }
        for (connector, connector_checkpoint) in &recovery.base_connector_checkpoints {
            if connector.is_empty()
                || connector_checkpoint.protocol_id.is_empty()
                || connector_checkpoint.format.is_empty()
                || connector_checkpoint.applied_at.records_through != recovery.base_frontier
            {
                return Err(SessionStoreError::InvalidRecord(
                    "base Connector checkpoint does not match base frontier".to_owned(),
                ));
            }
        }
        if let Some(verification) = &recovery.historical_replay
            && (verification.connector.is_empty()
                || verification.protocol.is_empty()
                || matches!(
                    (verification.from, verification.through),
                    (RecordFrontier::Through(from), RecordFrontier::Through(through)) if from > through
                ))
        {
            return Err(SessionStoreError::InvalidRecord(
                "historical replay range is reversed".to_owned(),
            ));
        }
        if self.lifecycle.is_empty()
            || self.supervisor.incarnation_nonce.is_empty()
            || self.supervisor.process_start_identity.is_empty()
        {
            return Err(SessionStoreError::InvalidRecord(
                "required session identity field is empty".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn workspace(&self) -> Result<&Path, SessionStoreError> {
        match self.materialization() {
            StoredLegacyV1Materialization::WorkspacePty { workspace } => Ok(workspace),
            StoredLegacyV1Materialization::ReadyState { .. } => {
                Err(SessionStoreError::InvalidRecord(
                    "Ready-State Session has no host workspace".to_string(),
                ))
            }
        }
    }

    pub fn upgrade_legacy_origin(&mut self, computation: &ComputationRef) {
        if matches!(
            &self.origin_computation,
            StoredComputationOrigin::LegacyV3State { .. }
        ) {
            self.origin_computation = StoredComputationOrigin::Native {
                computation_type: computation.computation_type.to_string(),
                object_ref: computation.object_ref.to_string(),
            };
        }
    }

    pub fn materialization(&self) -> &StoredLegacyV1Materialization {
        self.legacy_v1().0
    }

    pub fn materialization_mut(&mut self) -> &mut StoredLegacyV1Materialization {
        match &mut self.runtime_profile {
            StoredRuntimeProfile::LegacyV1 {
                materialization, ..
            } => materialization,
        }
    }

    pub fn recovery(&self) -> &StoredLegacyV1Recovery {
        self.legacy_v1().1
    }

    pub fn recovery_mut(&mut self) -> &mut StoredLegacyV1Recovery {
        match &mut self.runtime_profile {
            StoredRuntimeProfile::LegacyV1 { recovery, .. } => recovery,
        }
    }

    fn legacy_v1(&self) -> (&StoredLegacyV1Materialization, &StoredLegacyV1Recovery) {
        match &self.runtime_profile {
            StoredRuntimeProfile::LegacyV1 {
                materialization,
                recovery,
            } => (materialization, recovery),
        }
    }
}

impl StoredRuntimeProfile {
    pub fn legacy_v1(
        materialization: StoredLegacyV1Materialization,
        base_frontier: RecordFrontier,
        durable_frontier: DurableFrontier,
    ) -> Self {
        Self::LegacyV1 {
            materialization,
            recovery: StoredLegacyV1Recovery {
                durable_frontier,
                latest_consistent_frontier: None,
                base_frontier,
                base_connector_checkpoints: BTreeMap::new(),
                active_checkpoint: None,
                historical_replay: None,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapsuleProtocolSessionStore {
    root: PathBuf,
}

impl CapsuleProtocolSessionStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        let root = root.as_ref().to_path_buf();
        ensure_owner_only_store_supported()?;
        fs::create_dir_all(&root)?;
        set_directory_owner_only(&root)?;
        Ok(Self { root })
    }

    pub fn write(&self, session: &StoredProtocolSession) -> Result<(), SessionStoreError> {
        session.validate()?;
        if matches!(
            &session.origin_computation,
            StoredComputationOrigin::LegacyV3State { .. }
        ) {
            return Err(SessionStoreError::InvalidRecord(
                "Session Store v4 writes require a native computation origin".to_owned(),
            ));
        }
        let directory = self.root.join(session.session_id.as_str());
        fs::create_dir_all(&directory)?;
        set_directory_owner_only(&directory)?;
        write_atomic_owner_only(
            &directory.join("session.json"),
            &serde_json::to_vec_pretty(session)?,
        )
    }

    pub fn read(&self, session_id: &SessionId) -> Result<StoredProtocolSession, SessionStoreError> {
        let path = self.root.join(session_id.as_str()).join("session.json");
        let bytes = fs::read(path)?;
        let mut value: Value = serde_json::from_slice(&bytes)?;
        let schema_version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|version| u16::try_from(version).ok())
            .unwrap_or(0);
        if schema_version == 2 {
            let object = value.as_object_mut().ok_or_else(|| {
                SessionStoreError::InvalidRecord("session record is not an object".to_string())
            })?;
            let workspace = object.remove("workspace").ok_or_else(|| {
                SessionStoreError::InvalidRecord("legacy session workspace is missing".to_string())
            })?;
            object.insert(
                "schema_version".to_string(),
                Value::from(SESSION_STORE_SCHEMA_VERSION),
            );
            object.insert(
                "runtime_profile".to_string(),
                serde_json::json!({ "kind": "workspace_pty", "workspace": workspace }),
            );
        } else if schema_version != 3 && schema_version != SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedSchema(schema_version));
        }
        if schema_version == 2 || schema_version == 3 {
            let object = value.as_object_mut().ok_or_else(|| {
                SessionStoreError::InvalidRecord("session record is not an object".to_string())
            })?;
            let state_type = object.remove("state_type").ok_or_else(|| {
                SessionStoreError::InvalidRecord("legacy state type is missing".to_string())
            })?;
            let state_ref = object.remove("base_state").ok_or_else(|| {
                SessionStoreError::InvalidRecord("legacy base State is missing".to_string())
            })?;
            object.insert(
                "schema_version".to_string(),
                Value::from(SESSION_STORE_SCHEMA_VERSION),
            );
            object.insert(
                "origin_computation".to_string(),
                serde_json::json!({
                    "kind": "legacy_v3_state",
                    "state_type": state_type,
                    "state_ref": state_ref,
                }),
            );
        }
        migrate_legacy_v4_shape(&mut value)?;
        let session: StoredProtocolSession = serde_json::from_value(value)?;
        session.validate()?;
        if &session.session_id != session_id {
            return Err(SessionStoreError::InvalidRecord(
                "session id does not match its store path".to_owned(),
            ));
        }
        Ok(session)
    }

    pub fn list(&self) -> Result<Vec<StoredProtocolSession>, SessionStoreError> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(session_id) = SessionId::parse(name) else {
                continue;
            };
            match self.read(&session_id) {
                Ok(session) => sessions.push(session),
                Err(SessionStoreError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(SessionStoreError::UnsupportedSchema(_)) => {}
                Err(error) => return Err(error),
            }
        }
        sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(sessions)
    }
}

fn migrate_legacy_v4_shape(value: &mut Value) -> Result<(), SessionStoreError> {
    let object = value.as_object_mut().ok_or_else(|| {
        SessionStoreError::InvalidRecord("session record is not an object".to_owned())
    })?;
    if !object.contains_key("origin_computation")
        && let Some(origin) = object.remove("base_computation")
    {
        object.insert("origin_computation".to_owned(), origin);
    }

    let needs_nesting = object
        .get("runtime_profile")
        .and_then(|profile| profile.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "legacy_v1");
    if !needs_nesting {
        return Ok(());
    }

    let materialization = object.remove("runtime_profile").ok_or_else(|| {
        SessionStoreError::InvalidRecord("legacy runtime profile is missing".to_owned())
    })?;
    let mut take_required = |field: &str| {
        object.remove(field).ok_or_else(|| {
            SessionStoreError::InvalidRecord(format!("legacy recovery field {field} is missing"))
        })
    };
    let durable_frontier = take_required("durable_frontier")?;
    let latest_consistent_frontier = take_required("latest_consistent_frontier")?;
    let base_frontier = take_required("base_frontier")?;
    let active_checkpoint = take_required("active_checkpoint")?;
    let historical_replay = take_required("historical_replay")?;
    let base_connector_checkpoints = object
        .remove("base_connector_checkpoints")
        .unwrap_or_else(|| serde_json::json!({}));
    object.insert(
        "runtime_profile".to_owned(),
        serde_json::json!({
            "kind": "legacy_v1",
            "materialization": materialization,
            "recovery": {
                "durable_frontier": durable_frontier,
                "latest_consistent_frontier": latest_consistent_frontier,
                "base_frontier": base_frontier,
                "base_connector_checkpoints": base_connector_checkpoints,
                "active_checkpoint": active_checkpoint,
                "historical_replay": historical_replay,
            }
        }),
    );
    Ok(())
}

pub fn write_atomic_owner_only(path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
    ensure_owner_only_store_supported()?;
    let parent = path.parent().ok_or(SessionStoreError::InvalidStorePath)?;
    let mut nonce = [0_u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let temporary = parent.join(format!(".session-{}.tmp", hex_bytes(&nonce)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    set_file_create_owner_only(&mut options);
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(parent)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(SessionStoreError::Io)
}

#[cfg(unix)]
pub(crate) fn ensure_owner_only_store_supported() -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn ensure_owner_only_store_supported() -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "owner-only Protocol Session storage requires an implemented platform ACL backend",
    ))
}

#[cfg(unix)]
pub(crate) fn set_directory_owner_only(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub(crate) fn set_directory_owner_only(_path: &Path) -> Result<(), std::io::Error> {
    ensure_owner_only_store_supported()
}

#[cfg(unix)]
fn set_file_create_owner_only(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_file_create_owner_only(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlAuthorizationError {
    #[error("control request carries an invalid session secret")]
    InvalidSecret,
    #[error("control request targets a stale Supervisor incarnation")]
    StaleIncarnation,
}

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("Protocol Session Store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protocol Session Store JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session id must be 1..=255 ASCII alphanumeric, '-' or '_'")]
    InvalidSessionId,
    #[error("unsupported Protocol Session Store schema {0}")]
    UnsupportedSchema(u16),
    #[error("invalid Protocol Session record: {0}")]
    InvalidRecord(String),
    #[error("invalid Protocol Session Store path")]
    InvalidStorePath,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn stored_session(identity: SupervisorIdentity) -> StoredProtocolSession {
        StoredProtocolSession::new(NewStoredProtocolSession {
            session_id: SessionId::parse("session-1").expect("session id"),
            lifecycle: "running".to_owned(),
            origin_computation: StoredComputationOrigin::Native {
                computation_type: "ato.computation.test@1".to_owned(),
                object_ref: format!("blake3:{}", "a".repeat(64)),
            },
            runtime_profile: StoredRuntimeProfile::legacy_v1(
                StoredLegacyV1Materialization::WorkspacePty {
                    workspace: std::env::current_dir().expect("absolute current directory"),
                },
                crate::RecordFrontier::Origin,
                DurableFrontier {
                    records_through: crate::RecordFrontier::Through(42),
                    journal_through: crate::JournalLsn::new(8192),
                },
            ),
            supervisor: identity,
        })
    }

    #[cfg(unix)]
    fn legacy_v4_value(session: &StoredProtocolSession) -> Value {
        let mut value = serde_json::to_value(session).unwrap();
        let object = value.as_object_mut().unwrap();
        let origin = object.remove("origin_computation").unwrap();
        object.insert("base_computation".to_owned(), origin);
        let profile = object.remove("runtime_profile").unwrap();
        let profile = profile.as_object().unwrap();
        object.insert(
            "runtime_profile".to_owned(),
            profile["materialization"].clone(),
        );
        for field in [
            "durable_frontier",
            "latest_consistent_frontier",
            "base_frontier",
            "base_connector_checkpoints",
            "active_checkpoint",
            "historical_replay",
        ] {
            object.insert(field.to_owned(), profile["recovery"][field].clone());
        }
        value
    }

    #[cfg(unix)]
    #[test]
    fn session_store_round_trips_validated_record() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let expected = stored_session(supervisor.identity);
        store.write(&expected).expect("write session");
        let actual = store.read(&expected.session_id).expect("read session");
        assert_eq!(actual, expected);
        assert_eq!(store.list().expect("list sessions"), [expected]);
    }

    #[cfg(unix)]
    #[test]
    fn v4_record_keeps_protocol_v1_recovery_out_of_generic_run_fields() {
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let session = stored_session(supervisor.identity);
        let value = serde_json::to_value(session).unwrap();
        let object = value.as_object().unwrap();

        assert!(object.contains_key("origin_computation"));
        for legacy_field in [
            "base_computation",
            "durable_frontier",
            "latest_consistent_frontier",
            "base_frontier",
            "base_connector_checkpoints",
            "active_checkpoint",
            "historical_replay",
        ] {
            assert!(!object.contains_key(legacy_field));
        }
        assert_eq!(value["runtime_profile"]["kind"], "legacy_v1");
    }

    #[cfg(unix)]
    #[test]
    fn pre_separation_v4_record_migrates_to_legacy_v1_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let expected = stored_session(supervisor.identity);
        let value = legacy_v4_value(&expected);
        let path = directory.path().join("session-1");
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("session.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        assert_eq!(store.read(&expected.session_id).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn schema_v2_workspace_record_decodes_as_workspace_runtime_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let mut expected = stored_session(supervisor.identity);
        expected.origin_computation = StoredComputationOrigin::LegacyV3State {
            state_type: "ato.state.test@1".to_owned(),
            state_ref: format!("blake3:{}", "a".repeat(64)),
        };
        let mut value = legacy_v4_value(&expected);
        let object = value.as_object_mut().unwrap();
        object.insert("schema_version".to_string(), Value::from(2));
        object.remove("base_computation");
        object.insert("state_type".to_string(), Value::from("ato.state.test@1"));
        object.insert(
            "base_state".to_string(),
            Value::from(format!("blake3:{}", "a".repeat(64))),
        );
        let profile = object.remove("runtime_profile").unwrap();
        object.insert("workspace".to_string(), profile["workspace"].clone());
        let path = directory.path().join("session-1");
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("session.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        assert_eq!(store.read(&expected.session_id).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn schema_v3_record_decodes_as_legacy_computation_origin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let expected = stored_session(supervisor.identity);
        let mut value = legacy_v4_value(&expected);
        let object = value.as_object_mut().unwrap();
        object.insert("schema_version".to_string(), Value::from(3));
        object.remove("base_computation");
        object.insert("state_type".to_string(), Value::from("ato.state.test@1"));
        object.insert(
            "base_state".to_string(),
            Value::from(format!("blake3:{}", "b".repeat(64))),
        );
        let path = directory.path().join("session-1");
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("session.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();

        let actual = store.read(&expected.session_id).unwrap();
        assert_eq!(
            actual.origin_computation,
            StoredComputationOrigin::LegacyV3State {
                state_type: "ato.state.test@1".to_owned(),
                state_ref: format!("blake3:{}", "b".repeat(64)),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn v4_write_rejects_legacy_state_origin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let mut session = stored_session(supervisor.identity);
        session.origin_computation = StoredComputationOrigin::LegacyV3State {
            state_type: "ato.state.test@1".to_owned(),
            state_ref: format!("blake3:{}", "b".repeat(64)),
        };

        let error = store.write(&session).unwrap_err();

        assert!(error.to_string().contains("native computation origin"));
    }

    #[cfg(unix)]
    #[test]
    fn legacy_origin_upgrades_once_without_replacing_native_identity() {
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let mut session = stored_session(supervisor.identity);
        session.origin_computation = StoredComputationOrigin::LegacyV3State {
            state_type: "ato.state.test@1".to_owned(),
            state_ref: format!("blake3:{}", "b".repeat(64)),
        };
        let first = ComputationRef {
            computation_type: ComputationTypeId::parse("ato.computation.first@1").unwrap(),
            object_ref: ComputationObjectRef::parse(format!("blake3:{}", "c".repeat(64))).unwrap(),
        };
        let second = ComputationRef {
            computation_type: ComputationTypeId::parse("ato.computation.second@1").unwrap(),
            object_ref: ComputationObjectRef::parse(format!("blake3:{}", "d".repeat(64))).unwrap(),
        };

        session.upgrade_legacy_origin(&first);
        session.upgrade_legacy_origin(&second);

        assert_eq!(
            session.origin_computation,
            StoredComputationOrigin::Native {
                computation_type: first.computation_type.to_string(),
                object_ref: first.object_ref.to_string(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_skips_legacy_and_future_schema_entries() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let expected = stored_session(supervisor.identity);
        store.write(&expected).expect("write session");
        for (id, version) in [("legacy", 1), ("future", 99)] {
            let path = directory.path().join(id);
            fs::create_dir(&path).expect("create unsupported entry");
            fs::write(
                path.join("session.json"),
                serde_json::to_vec(&serde_json::json!({"schema_version": version})).unwrap(),
            )
            .expect("write unsupported entry");
        }
        assert_eq!(store.list().expect("list sessions"), [expected]);
    }

    #[cfg(unix)]
    #[test]
    fn connector_checkpoint_must_match_consistent_frontier() {
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let mut session = stored_session(supervisor.identity);
        let captured_at = session.recovery().durable_frontier;
        session.recovery_mut().latest_consistent_frontier = Some(captured_at);
        session.recovery_mut().active_checkpoint = Some(StoredLocalCheckpoint {
            state_ref: format!("blake3:{}", "b".repeat(64)),
            captured_at,
            workspace_digest: format!("blake3:{}", "b".repeat(64)),
            resume_fidelity: "filesystem_restart".to_owned(),
            connector_checkpoints: BTreeMap::from([(
                "terminal.main".to_owned(),
                StoredConnectorCheckpoint {
                    protocol_id: "ato.io.pty@1".to_owned(),
                    applied_at: DurableFrontier {
                        records_through: RecordFrontier::Through(41),
                        journal_through: captured_at.journal_through,
                    },
                    format: "ato.io.pty.local-checkpoint@1".to_owned(),
                    payload: serde_json::json!({"rows": 40, "cols": 132}),
                },
            )]),
        });
        assert!(matches!(
            session.validate(),
            Err(SessionStoreError::InvalidRecord(message))
                if message.contains("Connector checkpoint frontier")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ready_state_runtime_profile_validation_fails_closed() {
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        let mut session = stored_session(supervisor.identity);
        let valid = || StoredLegacyV1Materialization::ReadyState {
            backend_id: "fake".to_string(),
            ready_state_manifest_id: format!("blake3:{}", "b".repeat(64)),
            cas_root: std::env::current_dir().unwrap().join("cas"),
            overlay_root: std::env::current_dir().unwrap().join("overlay"),
            vmm_pid: Some(42),
            vmm_process_start_identity: Some("start-42".to_string()),
        };
        *session.materialization_mut() = valid();
        assert!(session.validate().is_ok());

        let invalid_profiles = [
            mutate_ready_state_profile(valid(), |vmm_pid, _, _, _, _, _| *vmm_pid = Some(0)),
            mutate_ready_state_profile(valid(), |_, identity, _, _, _, _| *identity = None),
            mutate_ready_state_profile(valid(), |_, _, manifest_id, _, _, _| {
                *manifest_id = "not-a-content-ref".to_string()
            }),
            mutate_ready_state_profile(valid(), |_, _, _, backend_id, _, _| {
                *backend_id = " ".to_string()
            }),
            mutate_ready_state_profile(valid(), |_, _, _, _, cas_root, _| {
                *cas_root = PathBuf::from("relative-cas")
            }),
            mutate_ready_state_profile(valid(), |_, _, _, _, _, overlay_root| {
                *overlay_root = PathBuf::from("relative-overlay")
            }),
        ];
        for profile in invalid_profiles {
            *session.materialization_mut() = profile;
            assert!(matches!(
                session.validate(),
                Err(SessionStoreError::InvalidRecord(message))
                    if message.contains("Ready-State runtime profile")
            ));
        }
    }

    #[cfg(unix)]
    fn mutate_ready_state_profile(
        mut profile: StoredLegacyV1Materialization,
        mutate: impl FnOnce(
            &mut Option<i32>,
            &mut Option<String>,
            &mut String,
            &mut String,
            &mut PathBuf,
            &mut PathBuf,
        ),
    ) -> StoredLegacyV1Materialization {
        let StoredLegacyV1Materialization::ReadyState {
            backend_id,
            ready_state_manifest_id,
            cas_root,
            overlay_root,
            vmm_pid,
            vmm_process_start_identity,
        } = &mut profile
        else {
            unreachable!()
        };
        mutate(
            vmm_pid,
            vmm_process_start_identity,
            ready_state_manifest_id,
            backend_id,
            cas_root,
            overlay_root,
        );
        profile
    }

    #[test]
    fn control_identity_rejects_wrong_secret_and_stale_generation() {
        let supervisor = NewSupervisorIdentity::generate(3, 100, "start-100");
        assert_eq!(
            supervisor.identity.authorize(
                b"wrong-secret",
                3,
                &supervisor.identity.incarnation_nonce,
                100,
                "start-100"
            ),
            Err(ControlAuthorizationError::InvalidSecret)
        );
        assert_eq!(
            supervisor.identity.authorize(
                supervisor.secret(),
                2,
                &supervisor.identity.incarnation_nonce,
                100,
                "start-100"
            ),
            Err(ControlAuthorizationError::StaleIncarnation)
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_directory_and_record_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("tempdir");
        let store = CapsuleProtocolSessionStore::open(directory.path()).expect("store");
        let supervisor = NewSupervisorIdentity::generate(1, 10, "start-10");
        let session = stored_session(supervisor.identity);
        store.write(&session).expect("write session");
        let session_directory = directory.path().join(session.session_id.as_str());
        let record = session_directory.join("session.json");
        assert_eq!(
            fs::metadata(session_directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(record)
                .expect("record metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn session_store_fails_closed_without_owner_only_acl_backend() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            CapsuleProtocolSessionStore::open(directory.path()),
            Err(SessionStoreError::Io(error))
                if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }
}
