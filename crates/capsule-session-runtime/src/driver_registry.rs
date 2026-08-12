use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use capsule_protocol::ProtocolId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AttachmentMechanism;
use crate::session_store::{
    ensure_owner_only_store_supported, set_directory_owner_only, write_atomic_owner_only,
};

const DRIVER_REGISTRY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverBindingProfile {
    StdioJsonRpcV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DriverExecutable {
    BuiltIn,
    External { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DriverTrust {
    AtoBuiltIn,
    TrustedPublisher { publisher: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverRegistration {
    pub protocol_id: String,
    pub implementation_id: String,
    pub implementation_version: String,
    pub binding: DriverBindingProfile,
    pub executable: DriverExecutable,
    pub trust: DriverTrust,
    pub attachment_mechanisms: Vec<AttachmentMechanism>,
    pub checkpoint_formats: Vec<String>,
}

impl DriverRegistration {
    fn validate(&self) -> Result<ProtocolId, DriverRegistryError> {
        let protocol_id = ProtocolId::parse(&self.protocol_id)
            .map_err(|error| DriverRegistryError::InvalidRegistration(error.to_string()))?;
        if self.implementation_id.is_empty()
            || self.implementation_version.is_empty()
            || self.attachment_mechanisms.is_empty()
        {
            return Err(DriverRegistryError::InvalidRegistration(
                "implementation identity and attachment mechanisms are required".to_owned(),
            ));
        }
        match (&self.executable, &self.trust) {
            (DriverExecutable::BuiltIn, DriverTrust::AtoBuiltIn) => {}
            (DriverExecutable::External { path }, DriverTrust::TrustedPublisher { publisher }) => {
                if !path.is_absolute() {
                    return Err(DriverRegistryError::RelativeExecutable(path.clone()));
                }
                if publisher.is_empty() {
                    return Err(DriverRegistryError::InvalidRegistration(
                        "trusted publisher is empty".to_owned(),
                    ));
                }
            }
            _ => return Err(DriverRegistryError::TrustExecutableMismatch),
        }
        Ok(protocol_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegistryDocument {
    schema_version: u16,
    registrations: BTreeMap<String, DriverRegistration>,
}

#[derive(Debug, Clone)]
pub struct DriverRegistry {
    path: PathBuf,
    registrations: BTreeMap<String, DriverRegistration>,
}

impl DriverRegistry {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DriverRegistryError> {
        let root = root.as_ref();
        ensure_owner_only_store_supported()?;
        fs::create_dir_all(root)?;
        set_directory_owner_only(root)?;
        let path = root.join("drivers.json");
        if !path.exists() {
            return Ok(Self {
                path,
                registrations: BTreeMap::new(),
            });
        }
        let document: RegistryDocument = serde_json::from_slice(&fs::read(&path)?)?;
        if document.schema_version != DRIVER_REGISTRY_SCHEMA_VERSION {
            return Err(DriverRegistryError::UnsupportedSchema(
                document.schema_version,
            ));
        }
        for (key, registration) in &document.registrations {
            let protocol_id = registration.validate()?;
            if key != protocol_id.as_str() {
                return Err(DriverRegistryError::InvalidRegistration(
                    "registry key does not match ProtocolId".to_owned(),
                ));
            }
        }
        Ok(Self {
            path,
            registrations: document.registrations,
        })
    }

    pub fn install(&mut self, registration: DriverRegistration) -> Result<(), DriverRegistryError> {
        let protocol_id = registration.validate()?;
        self.registrations
            .insert(protocol_id.to_string(), registration);
        self.persist()
    }

    pub fn resolve(
        &self,
        protocol_id: &ProtocolId,
    ) -> Result<&DriverRegistration, DriverRegistryError> {
        self.registrations
            .get(protocol_id.as_str())
            .ok_or_else(|| DriverRegistryError::Unavailable(protocol_id.clone()))
    }

    fn persist(&self) -> Result<(), DriverRegistryError> {
        let document = RegistryDocument {
            schema_version: DRIVER_REGISTRY_SCHEMA_VERSION,
            registrations: self.registrations.clone(),
        };
        write_atomic_owner_only(&self.path, &serde_json::to_vec_pretty(&document)?)
            .map_err(|error| DriverRegistryError::Store(error.to_string()))
    }
}

#[derive(Debug, Error)]
pub enum DriverRegistryError {
    #[error("Driver Registry I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Driver Registry JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported Driver Registry schema {0}")]
    UnsupportedSchema(u16),
    #[error("invalid Driver registration: {0}")]
    InvalidRegistration(String),
    #[error("external Driver executable path must be absolute: {}", .0.display())]
    RelativeExecutable(PathBuf),
    #[error("Driver trust does not match executable kind")]
    TrustExecutableMismatch,
    #[error("no explicitly installed Driver for Protocol {0}")]
    Unavailable(ProtocolId),
    #[error("Driver Registry store failed: {0}")]
    Store(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn protocol() -> ProtocolId {
        ProtocolId::parse("com.example.io.echo@1").expect("protocol")
    }

    #[cfg(unix)]
    fn external_registration(path: PathBuf) -> DriverRegistration {
        DriverRegistration {
            protocol_id: protocol().to_string(),
            implementation_id: "com.example.driver.echo".to_owned(),
            implementation_version: "1.0.0".to_owned(),
            binding: DriverBindingProfile::StdioJsonRpcV1,
            executable: DriverExecutable::External { path },
            trust: DriverTrust::TrustedPublisher {
                publisher: "com.example".to_owned(),
            },
            attachment_mechanisms: vec![AttachmentMechanism::TcpProxy],
            checkpoint_formats: Vec::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unknown_protocol_fails_closed_without_path_search() {
        let directory = tempfile::tempdir().expect("tempdir");
        let registry = DriverRegistry::open(directory.path()).expect("registry");
        assert!(matches!(
            registry.resolve(&protocol()),
            Err(DriverRegistryError::Unavailable(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn external_driver_requires_explicit_absolute_path_and_round_trips() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut registry = DriverRegistry::open(directory.path()).expect("registry");
        assert!(matches!(
            registry.install(external_registration(PathBuf::from("echo-driver"))),
            Err(DriverRegistryError::RelativeExecutable(_))
        ));

        let registration = external_registration(directory.path().join("echo-driver"));
        registry
            .install(registration.clone())
            .expect("install Driver");
        let reopened = DriverRegistry::open(directory.path()).expect("reopen registry");
        assert_eq!(
            reopened.resolve(&protocol()).expect("resolve"),
            &registration
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn registry_fails_closed_without_owner_only_acl_backend() {
        let directory = tempfile::tempdir().expect("tempdir");
        assert!(matches!(
            DriverRegistry::open(directory.path()),
            Err(DriverRegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }
}
