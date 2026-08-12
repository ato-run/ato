use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use capsule_protocol::{ContentRef, StateTypeId};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::RecordFrontier;

const SESSION_STORE_SCHEMA_VERSION: u16 = 1;
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
pub struct StoredProtocolSession {
    pub schema_version: u16,
    pub session_id: SessionId,
    pub lifecycle: String,
    pub state_type: String,
    pub state_ref: String,
    pub committed_frontier: RecordFrontier,
    pub supervisor: SupervisorIdentity,
}

impl StoredProtocolSession {
    pub fn new(
        session_id: SessionId,
        lifecycle: impl Into<String>,
        state_type: &StateTypeId,
        state_ref: &ContentRef,
        committed_frontier: RecordFrontier,
        supervisor: SupervisorIdentity,
    ) -> Self {
        Self {
            schema_version: SESSION_STORE_SCHEMA_VERSION,
            session_id,
            lifecycle: lifecycle.into(),
            state_type: state_type.to_string(),
            state_ref: state_ref.to_string(),
            committed_frontier,
            supervisor,
        }
    }

    fn validate(&self) -> Result<(), SessionStoreError> {
        if self.schema_version != SESSION_STORE_SCHEMA_VERSION {
            return Err(SessionStoreError::UnsupportedSchema(self.schema_version));
        }
        SessionId::parse(self.session_id.to_string())?;
        StateTypeId::parse(&self.state_type)
            .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
        ContentRef::parse(&self.state_ref)
            .map_err(|error| SessionStoreError::InvalidRecord(error.to_string()))?;
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
}

#[derive(Debug, Clone)]
pub struct CapsuleProtocolSessionStore {
    root: PathBuf,
}

impl CapsuleProtocolSessionStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        set_directory_owner_only(&root)?;
        Ok(Self { root })
    }

    pub fn write(&self, session: &StoredProtocolSession) -> Result<(), SessionStoreError> {
        session.validate()?;
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
        let session: StoredProtocolSession = serde_json::from_slice(&fs::read(path)?)?;
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
                Err(error) => return Err(error),
            }
        }
        sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        Ok(sessions)
    }
}

pub(crate) fn write_atomic_owner_only(path: &Path, bytes: &[u8]) -> Result<(), SessionStoreError> {
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
pub(crate) fn set_directory_owner_only(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub(crate) fn set_directory_owner_only(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
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

    fn stored_session(identity: SupervisorIdentity) -> StoredProtocolSession {
        StoredProtocolSession::new(
            SessionId::parse("session-1").expect("session id"),
            "running",
            &StateTypeId::parse("ato.state.test@1").expect("state type"),
            &ContentRef::parse(format!("blake3:{}", "a".repeat(64))).expect("state ref"),
            RecordFrontier::Through(42),
            identity,
        )
    }

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
}
