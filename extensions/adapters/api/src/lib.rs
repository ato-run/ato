//! Shared public factory/session contract used identically by every Adapter.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use ato_computation::{PortId, ProtocolId};
use ato_objects::{Direction, ObjectStore, RecordEnvelope, RecordId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCapturePolicy {
    includes: Vec<String>,
    excludes: Vec<String>,
}

impl WorkspaceCapturePolicy {
    pub fn secure_default() -> Self {
        Self {
            includes: Vec::new(),
            excludes: Vec::new(),
        }
    }

    pub fn new(includes: Vec<String>, excludes: Vec<String>) -> Result<Self, AdapterError> {
        for path in includes.iter().chain(&excludes) {
            validate_capture_path(path)?;
        }
        Ok(Self { includes, excludes })
    }

    pub fn captures(&self, relative: &Path) -> bool {
        let Some(path) = normalized_relative(relative) else {
            return false;
        };
        !securely_excluded(&path)
            && !self.excludes.iter().any(|entry| under(&path, entry))
            && (self.includes.is_empty() || self.includes.iter().any(|entry| under(&path, entry)))
    }

    pub fn descends_into(&self, relative: &Path) -> bool {
        let Some(path) = normalized_relative(relative) else {
            return false;
        };
        !securely_excluded(&path)
            && !self.excludes.iter().any(|entry| under(&path, entry))
            && (self.includes.is_empty()
                || self
                    .includes
                    .iter()
                    .any(|entry| under(&path, entry) || under(entry, &path)))
    }
}

impl Default for WorkspaceCapturePolicy {
    fn default() -> Self {
        Self::secure_default()
    }
}

fn validate_capture_path(path: &str) -> Result<(), AdapterError> {
    let value = Path::new(path);
    if path.is_empty()
        || value.is_absolute()
        || value
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(AdapterError::InvalidConfig(format!(
            "workspace capture path `{path}` must be a rooted-relative normal path"
        )));
    }
    Ok(())
}

fn normalized_relative(path: &Path) -> Option<String> {
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(
        path.components()
            .map(|part| part.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()?
            .join("/"),
    )
}

fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn securely_excluded(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            ".capsule" | ".git" | ".ssh" | ".aws" | ".gnupg"
        )
    }) {
        return true;
    }
    let file = parts
        .last()
        .copied()
        .unwrap_or_default()
        .to_ascii_lowercase();
    file == ".env"
        || file.starts_with(".env.")
        || file == "credentials"
        || file.starts_with("credentials.")
        || matches!(file.as_str(), "id_rsa" | "id_ed25519")
        || [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|suffix| file.ends_with(suffix))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureConsistency {
    #[default]
    Unsupported,
    /// The workload declares that semantic changes cross attached Adapter
    /// boundaries. The barrier drains those boundaries but does not freeze
    /// arbitrary background process state.
    AdapterMediated,
    /// The runtime freezes all state producers while the frontier is captured.
    RuntimeFrozen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdapterCapabilities {
    pub observe: bool,
    pub apply: bool,
    pub verify: bool,
    pub quiesce: bool,
    pub capture_consistency: CaptureConsistency,
}

#[derive(Debug, Default)]
struct CaptureGateState {
    paused: bool,
    in_flight: usize,
}

/// A small runtime-only admission gate shared by live Adapters. Work obtains a
/// permit immediately before it crosses an observable boundary. Capture closes
/// admission and waits for all already-admitted work to leave.
#[derive(Debug, Default)]
pub struct CaptureGate {
    state: Mutex<CaptureGateState>,
    changed: Condvar,
}

impl CaptureGate {
    pub fn enter(&self) -> Result<CapturePermit<'_>, AdapterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdapterError::Operation("capture gate was poisoned".to_owned()))?;
        while state.paused {
            state = self
                .changed
                .wait(state)
                .map_err(|_| AdapterError::Operation("capture gate was poisoned".to_owned()))?;
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .ok_or_else(|| AdapterError::Operation("capture gate overflow".to_owned()))?;
        Ok(CapturePermit { gate: self })
    }

    pub fn pause_and_drain(&self) -> Result<(), AdapterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdapterError::Operation("capture gate was poisoned".to_owned()))?;
        state.paused = true;
        while state.in_flight != 0 {
            state = self
                .changed
                .wait(state)
                .map_err(|_| AdapterError::Operation("capture gate was poisoned".to_owned()))?;
        }
        Ok(())
    }

    pub fn resume(&self) -> Result<(), AdapterError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AdapterError::Operation("capture gate was poisoned".to_owned()))?;
        state.paused = false;
        self.changed.notify_all();
        Ok(())
    }

    fn leave(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight = state.in_flight.saturating_sub(1);
            self.changed.notify_all();
        }
    }
}

pub struct CapturePermit<'a> {
    gate: &'a CaptureGate,
}

impl Drop for CapturePermit<'_> {
    fn drop(&mut self) {
        self.gate.leave();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterObservation {
    pub adapter_id: String,
    pub protocol_id: ProtocolId,
    pub port_id: PortId,
    pub direction: Direction,
    pub payload: Vec<u8>,
    pub caused_by: Vec<RecordId>,
    pub effect: ObservationEffect,
}

/// Whether an observation is only evidence about a realization or commits a
/// semantic transition of the residual computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationEffect {
    Evidence,
    Evolution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterInstance {
    pub instance_id: String,
    pub adapter_id: String,
    #[serde(default)]
    pub config: Value,
}

pub struct AdapterContext<'a> {
    pub workspace: &'a Path,
    pub objects: &'a dyn ObjectStore,
}

pub struct AdapterAttachContext<'a> {
    pub runtime: AdapterContext<'a>,
    pub observations: Arc<dyn ObservationSink>,
}

pub trait ObservationSink: Send + Sync {
    fn emit(&self, observation: AdapterObservation) -> Result<(), AdapterError>;
}

#[derive(Default)]
pub struct IgnoreObservations;

impl ObservationSink for IgnoreObservations {
    fn emit(&self, _observation: AdapterObservation) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// One live Adapter instance. Stateful operations always target this object,
/// including replay, quiesce, and detach.
pub trait AttachedAdapter: Send {
    fn instance_id(&self) -> &str;
    fn adapter_id(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;

    fn accepts(&self, record: &RecordEnvelope) -> bool {
        self.adapter_id() == record.adapter_id
    }

    fn apply(
        &mut self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "apply",
        })
    }

    fn verify(
        &mut self,
        _record: &RecordEnvelope,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "verify",
        })
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn pause_for_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "capture_barrier",
        })
    }

    fn resume_after_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "capture_barrier_release",
        })
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn wait(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    fn activate(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Creates configured live instances. Built-ins and third parties enter the
/// runtime through this exact contract; the supervisor never switches on IDs.
pub trait AdapterFactory: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;

    fn preflight(
        &self,
        _instance: &AdapterInstance,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError>;
}

#[derive(Default)]
pub struct AdapterRegistry {
    factories: BTreeMap<String, Arc<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    pub fn register(&mut self, factory: Arc<dyn AdapterFactory>) -> Result<(), AdapterError> {
        let id = validate_adapter_id(factory.id())?;
        if self.factories.insert(id.clone(), factory).is_some() {
            return Err(AdapterError::Duplicate(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn AdapterFactory, AdapterError> {
        self.factories
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| AdapterError::Unknown(id.to_owned()))
    }

    pub fn attach_all(
        &self,
        instances: &[AdapterInstance],
        context: &AdapterAttachContext<'_>,
    ) -> Result<Vec<Box<dyn AttachedAdapter>>, AdapterError> {
        for instance in instances {
            if instance.adapter_id != self.get(&instance.adapter_id)?.id() {
                return Err(AdapterError::InvalidConfig(
                    "factory id does not match configured adapter".to_owned(),
                ));
            }
            self.get(&instance.adapter_id)?
                .preflight(instance, &context.runtime)?;
        }
        let mut attached: Vec<Box<dyn AttachedAdapter>> = Vec::new();
        for instance in instances {
            match self.get(&instance.adapter_id)?.attach(instance, context) {
                Ok(session) => attached.push(session),
                Err(error) => {
                    for session in attached.iter_mut().rev() {
                        let _ = session.detach(&context.runtime);
                    }
                    return Err(error);
                }
            }
        }
        Ok(attached)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.factories.keys().map(String::as_str)
    }
}

fn validate_adapter_id(value: &str) -> Result<String, AdapterError> {
    let (name, version) = value
        .rsplit_once('@')
        .ok_or_else(|| AdapterError::InvalidId(value.to_owned()))?;
    if !name.contains('.')
        || name.is_empty()
        || version.is_empty()
        || !version.bytes().all(|byte| byte.is_ascii_digit())
        || version.starts_with('0')
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err(AdapterError::InvalidId(value.to_owned()));
    }
    Ok(value.to_owned())
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("invalid adapter id `{0}`")]
    InvalidId(String),
    #[error("adapter `{0}` is already registered")]
    Duplicate(String),
    #[error("adapter `{0}` is not registered")]
    Unknown(String),
    #[error("adapter `{adapter}` does not support {operation}")]
    Unsupported {
        adapter: String,
        operation: &'static str,
    },
    #[error("invalid adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("adapter operation failed: {0}")]
    Operation(String),
    #[error(transparent)]
    Objects(#[from] ato_objects::ObjectError),
    #[error("adapter JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("adapter I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ThirdParty;
    struct ThirdPartySession(String);

    impl AttachedAdapter for ThirdPartySession {
        fn instance_id(&self) -> &str {
            &self.0
        }
        fn adapter_id(&self) -> &str {
            "example.custom@1"
        }
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                observe: true,
                verify: true,
                ..AdapterCapabilities::default()
            }
        }
    }

    impl AdapterFactory for ThirdParty {
        fn id(&self) -> &str {
            "example.custom@1"
        }
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                observe: true,
                verify: true,
                ..AdapterCapabilities::default()
            }
        }
        fn attach(
            &self,
            instance: &AdapterInstance,
            _: &AdapterAttachContext<'_>,
        ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
            Ok(Box::new(ThirdPartySession(instance.instance_id.clone())))
        }
    }

    #[test]
    fn third_party_uses_the_same_factory_and_live_session_path() {
        let directory = tempfile::tempdir().unwrap();
        let objects = ato_objects::FsObjectStore::open(directory.path().join("objects")).unwrap();
        let mut registry = AdapterRegistry::default();
        registry.register(Arc::new(ThirdParty)).unwrap();
        let sessions = registry
            .attach_all(
                &[AdapterInstance {
                    instance_id: "custom.one".to_owned(),
                    adapter_id: "example.custom@1".to_owned(),
                    config: Value::Null,
                }],
                &AdapterAttachContext {
                    runtime: AdapterContext {
                        workspace: directory.path(),
                        objects: &objects,
                    },
                    observations: Arc::new(IgnoreObservations),
                },
            )
            .unwrap();
        assert_eq!(sessions[0].instance_id(), "custom.one");
        assert!(sessions[0].capabilities().observe);
    }

    #[test]
    fn workspace_capture_policy_is_explicit_and_secure_by_default() {
        let policy = WorkspaceCapturePolicy::new(
            vec!["src".to_owned(), "config".to_owned()],
            vec!["src/generated".to_owned()],
        )
        .unwrap();
        assert!(policy.captures(Path::new("src/main.rs")));
        assert!(!policy.captures(Path::new("src/generated/key.txt")));
        assert!(!policy.captures(Path::new("README.md")));
        assert!(!policy.captures(Path::new("config/.env")));
        assert!(!policy.captures(Path::new("config/signing.key")));
    }

    #[test]
    fn capture_gate_drains_admitted_work_and_reopens() {
        let gate = Arc::new(CaptureGate::default());
        let permit = gate.enter().unwrap();
        let waiting = Arc::clone(&gate);
        let paused = std::thread::spawn(move || waiting.pause_and_drain());
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!paused.is_finished());
        drop(permit);
        paused.join().unwrap().unwrap();
        gate.resume().unwrap();
        drop(gate.enter().unwrap());
    }
}
