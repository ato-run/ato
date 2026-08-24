//! Shared public factory/session contract used identically by every Adapter.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use ato_computation::{OperationId, PortId, ProtocolId};
use ato_objects::{
    Direction, ObjectStore, RecordCandidate, RecordEnvelope, RecordEnvelopeV2, RecordId,
    read_exact_object,
};
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
pub struct AdapterCapabilities {
    pub observe: bool,
    pub apply: bool,
    pub verify: bool,
    pub quiesce: bool,
}

/// Operation-level execution requirement derived from a Record closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationRequirement {
    pub protocol_id: ProtocolId,
    pub operation_id: OperationId,
    pub payload_version: u32,
    pub required_features: BTreeSet<String>,
}

impl From<&RecordEnvelopeV2> for OperationRequirement {
    fn from(record: &RecordEnvelopeV2) -> Self {
        Self {
            protocol_id: record.protocol_id.clone(),
            operation_id: record.operation_id.clone(),
            payload_version: record.payload_version,
            required_features: record.required_features.clone(),
        }
    }
}

/// One Protocol operation that an Actuator Provider can provision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportedOperation {
    pub protocol_id: ProtocolId,
    pub operation_id: OperationId,
    pub payload_version: u32,
    pub required_features: BTreeSet<String>,
}

impl SupportedOperation {
    pub fn new(
        protocol_id: impl Into<String>,
        operation_id: impl Into<String>,
        payload_version: u32,
        required_features: BTreeSet<String>,
    ) -> Result<Self, AdapterError> {
        if payload_version == 0 {
            return Err(AdapterError::InvalidConfig(
                "payload version must be positive".to_owned(),
            ));
        }
        Ok(Self {
            protocol_id: ProtocolId::parse(protocol_id.into())
                .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
            operation_id: OperationId::parse(operation_id.into())
                .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
            payload_version,
            required_features,
        })
    }

    pub fn supports(&self, requirement: &OperationRequirement) -> bool {
        self.protocol_id == requirement.protocol_id
            && self.operation_id == requirement.operation_id
            && self.payload_version == requirement.payload_version
            && requirement
                .required_features
                .is_subset(&self.required_features)
    }
}

/// Planner-selected binding from one logical Port to an Actuator Provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBinding {
    pub port_id: PortId,
    pub provider_id: String,
    pub route_id: String,
}

/// Deterministic provisionable route selected for one Record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActuatorRoute {
    pub provider_id: String,
    pub route_id: String,
    pub port_id: PortId,
    pub operation: SupportedOperation,
}

/// A Stylus turns a physical interaction into a RecordCandidate and submits it
/// to the recording boundary. Implementations must not perform persistence.
pub trait Stylus: Send + Sync {
    fn record(&self, candidate: RecordCandidate) -> Result<(), AdapterError>;
}

/// A live operation is the non-persistent counterpart of a portable Record.
/// Runner ingress may apply it directly; Player reaches the same Adapter
/// boundary only after decoding a stored Record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOperation {
    pub protocol_id: ProtocolId,
    pub operation_id: OperationId,
    pub port_id: PortId,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub struct IgnoreRecords;

impl Stylus for IgnoreRecords {
    fn record(&self, _candidate: RecordCandidate) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// One provisioned handler for operation-based Records.
pub trait Actuator: Send {
    fn route(&self) -> &ActuatorRoute;

    fn apply(
        &mut self,
        record: &RecordEnvelopeV2,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError>;
}

/// Extension boundary capable of provisioning Actuators in a target
/// Environment or Realization.
pub trait ActuatorProvider: Send + Sync {
    fn id(&self) -> &str;
    fn supported_operations(&self) -> &[SupportedOperation];

    fn routes(
        &self,
        requirement: &OperationRequirement,
        port_id: &PortId,
        binding: Option<&PortBinding>,
        _context: &AdapterContext<'_>,
    ) -> Result<Vec<ActuatorRoute>, AdapterError> {
        if binding.is_some_and(|binding| binding.provider_id != self.id()) {
            return Ok(Vec::new());
        }
        Ok(self
            .supported_operations()
            .iter()
            .filter(|operation| operation.supports(requirement))
            .cloned()
            .map(|operation| ActuatorRoute {
                provider_id: self.id().to_owned(),
                route_id: binding
                    .map_or_else(|| self.id().to_owned(), |binding| binding.route_id.clone()),
                port_id: port_id.clone(),
                operation,
            })
            .collect())
    }

    /// Validates the payload schema without provisioning or applying a route.
    fn validate_payload(
        &self,
        record: &RecordEnvelopeV2,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError>;

    fn provision(
        &self,
        route: &ActuatorRoute,
        context: &AdapterContext<'_>,
    ) -> Result<Box<dyn Actuator>, AdapterError>;
}

#[derive(Default)]
pub struct ActuatorProviderRegistry {
    providers: BTreeMap<String, Arc<dyn ActuatorProvider>>,
}

impl ActuatorProviderRegistry {
    pub fn register(&mut self, provider: Arc<dyn ActuatorProvider>) -> Result<(), AdapterError> {
        let id = validate_adapter_id(provider.id())?;
        if self.providers.insert(id.clone(), provider).is_some() {
            return Err(AdapterError::DuplicateProvider(id));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<&dyn ActuatorProvider, AdapterError> {
        self.providers
            .get(id)
            .map(Arc::as_ref)
            .ok_or_else(|| AdapterError::UnknownProvider(id.to_owned()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn ActuatorProvider> {
        self.providers.values().map(Arc::as_ref)
    }
}

pub const ADAPTER_PROTOCOL_ID: &str = "ato.adapter@1";
pub const ADAPTER_ADD_OPERATION: &str = "add";
pub const ADAPTER_REMOVE_OPERATION: &str = "remove";
pub const ADAPTER_CONFIGURE_OPERATION: &str = "configure";
const MAX_ADAPTER_CONTROL_PAYLOAD_BYTES: u64 = 1024 * 1024;

/// Payload interpreted by the built-in Adapter lifecycle provider, never by
/// Player Core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdapterControlPayload {
    Add {
        instance_id: String,
        implementation_id: String,
        #[serde(default)]
        config: Value,
    },
    Remove {
        instance_id: String,
    },
    Configure {
        instance_id: String,
        #[serde(default)]
        config: Value,
    },
}

pub fn encode_adapter_control_payload(
    payload: &AdapterControlPayload,
) -> Result<Vec<u8>, AdapterError> {
    Ok(serde_jcs::to_vec(payload)?)
}

pub fn decode_adapter_control_payload(bytes: &[u8]) -> Result<AdapterControlPayload, AdapterError> {
    let payload = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&payload)? != bytes {
        return Err(AdapterError::InvalidPayload(
            "Adapter control payload is not canonical JCS".to_owned(),
        ));
    }
    Ok(payload)
}

/// Extension-owned implementation of Adapter lifecycle semantics.
pub trait AdapterControlPlane: Send + Sync {
    fn add(
        &self,
        instance_id: &str,
        implementation_id: &str,
        config: &Value,
    ) -> Result<(), AdapterError>;
    fn remove(&self, instance_id: &str) -> Result<(), AdapterError>;
    fn configure(&self, instance_id: &str, config: &Value) -> Result<(), AdapterError>;
}

pub struct AdapterControlActuatorProvider {
    id: String,
    operations: Vec<SupportedOperation>,
    control: Arc<dyn AdapterControlPlane>,
}

impl AdapterControlActuatorProvider {
    pub fn new(
        id: impl Into<String>,
        control: Arc<dyn AdapterControlPlane>,
    ) -> Result<Self, AdapterError> {
        let id = validate_adapter_id(&id.into())?;
        let protocol_id = ProtocolId::parse(ADAPTER_PROTOCOL_ID)
            .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
        let operations = [
            ADAPTER_ADD_OPERATION,
            ADAPTER_REMOVE_OPERATION,
            ADAPTER_CONFIGURE_OPERATION,
        ]
        .into_iter()
        .map(|operation| {
            Ok(SupportedOperation {
                protocol_id: protocol_id.clone(),
                operation_id: OperationId::parse(operation)
                    .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
                payload_version: 1,
                required_features: BTreeSet::new(),
            })
        })
        .collect::<Result<_, AdapterError>>()?;
        Ok(Self {
            id,
            operations,
            control,
        })
    }
}

impl ActuatorProvider for AdapterControlActuatorProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn supported_operations(&self) -> &[SupportedOperation] {
        &self.operations
    }

    fn validate_payload(
        &self,
        record: &RecordEnvelopeV2,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let payload = read_control_payload(record, context)?;
        if payload_operation(&payload) != record.operation_id.as_str() {
            return Err(AdapterError::InvalidPayload(format!(
                "payload kind does not match operation `{}`",
                record.operation_id
            )));
        }
        Ok(())
    }

    fn provision(
        &self,
        route: &ActuatorRoute,
        _context: &AdapterContext<'_>,
    ) -> Result<Box<dyn Actuator>, AdapterError> {
        Ok(Box::new(AdapterControlActuator {
            route: route.clone(),
            control: Arc::clone(&self.control),
        }))
    }
}

struct AdapterControlActuator {
    route: ActuatorRoute,
    control: Arc<dyn AdapterControlPlane>,
}

impl Actuator for AdapterControlActuator {
    fn route(&self) -> &ActuatorRoute {
        &self.route
    }

    fn apply(
        &mut self,
        record: &RecordEnvelopeV2,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        match read_control_payload(record, context)? {
            AdapterControlPayload::Add {
                instance_id,
                implementation_id,
                config,
            } => self.control.add(&instance_id, &implementation_id, &config),
            AdapterControlPayload::Remove { instance_id } => self.control.remove(&instance_id),
            AdapterControlPayload::Configure {
                instance_id,
                config,
            } => self.control.configure(&instance_id, &config),
        }
    }
}

fn read_control_payload(
    record: &RecordEnvelopeV2,
    context: &AdapterContext<'_>,
) -> Result<AdapterControlPayload, AdapterError> {
    let metadata = context.objects.metadata(&record.payload_ref)?;
    let bytes = read_exact_object(
        context.objects,
        &record.payload_ref,
        metadata.size,
        MAX_ADAPTER_CONTROL_PAYLOAD_BYTES,
    )?;
    decode_adapter_control_payload(&bytes)
}

fn payload_operation(payload: &AdapterControlPayload) -> &'static str {
    match payload {
        AdapterControlPayload::Add { .. } => ADAPTER_ADD_OPERATION,
        AdapterControlPayload::Remove { .. } => ADAPTER_REMOVE_OPERATION,
        AdapterControlPayload::Configure { .. } => ADAPTER_CONFIGURE_OPERATION,
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
    /// v2 operation path. Stylus submission must not perform persistence.
    pub stylus: Arc<dyn Stylus>,
    /// Legacy v1 compatibility path. New implementations must not send
    /// non-applicable output through the v2 Stylus.
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

    fn apply_operation(&mut self, _operation: &LiveOperation) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported {
            adapter: self.adapter_id().to_owned(),
            operation: "apply_operation",
        })
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

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    fn wait(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    fn activate(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Publishes externally reachable Surfaces after Contract acceptance.
    /// Attach and activate implementations must keep those Surfaces hidden.
    fn publish(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
}

/// Creates configured live instances. Built-ins and third parties enter the
/// runtime through this exact contract; the supervisor never switches on IDs.
pub trait AdapterFactory: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;

    fn supported_operations(&self) -> Vec<SupportedOperation> {
        Vec::new()
    }

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
    #[error("Actuator Provider `{0}` is already registered")]
    DuplicateProvider(String),
    #[error("Actuator Provider `{0}` is not registered")]
    UnknownProvider(String),
    #[error("adapter `{adapter}` does not support {operation}")]
    Unsupported {
        adapter: String,
        operation: &'static str,
    },
    #[error("invalid adapter configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid Record payload: {0}")]
    InvalidPayload(String),
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
    use std::sync::Mutex;

    use ato_computation::OperationId;
    use ato_objects::{MemoryObjectStore, RecordBodyV2};

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
                    stylus: Arc::new(IgnoreRecords),
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

    #[derive(Default)]
    struct TestControl {
        calls: Mutex<Vec<String>>,
    }

    impl AdapterControlPlane for TestControl {
        fn add(
            &self,
            instance_id: &str,
            implementation_id: &str,
            _config: &Value,
        ) -> Result<(), AdapterError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("add:{instance_id}:{implementation_id}"));
            Ok(())
        }

        fn remove(&self, instance_id: &str) -> Result<(), AdapterError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("remove:{instance_id}"));
            Ok(())
        }

        fn configure(&self, instance_id: &str, _config: &Value) -> Result<(), AdapterError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("configure:{instance_id}"));
            Ok(())
        }
    }

    fn control_record(
        objects: &MemoryObjectStore,
        operation: &str,
        payload: AdapterControlPayload,
    ) -> RecordEnvelopeV2 {
        let payload_ref = objects
            .put(&encode_adapter_control_payload(&payload).unwrap())
            .unwrap();
        RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse(ADAPTER_PROTOCOL_ID).unwrap(),
            operation_id: OperationId::parse(operation).unwrap(),
            port_id: PortId::parse("adapters.main").unwrap(),
            payload_ref,
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: None,
            stream: "main".to_owned(),
            local_seq: 1,
            writer_order: 1,
            caused_by: Vec::new(),
            observed_at: "2030-01-01T00:00:00Z".to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn adapter_control_semantics_are_implemented_by_provider_not_player() {
        let objects = MemoryObjectStore::default();
        let control = Arc::new(TestControl::default());
        let provider = AdapterControlActuatorProvider::new(
            "ato.adapter.control@1",
            Arc::clone(&control) as Arc<dyn AdapterControlPlane>,
        )
        .unwrap();
        let record = control_record(
            &objects,
            ADAPTER_ADD_OPERATION,
            AdapterControlPayload::Add {
                instance_id: "browser.main".to_owned(),
                implementation_id: "browser.firefox@1".to_owned(),
                config: Value::Null,
            },
        );
        let context = AdapterContext {
            workspace: Path::new("."),
            objects: &objects,
        };
        provider.validate_payload(&record, &context).unwrap();
        let route = provider
            .routes(
                &OperationRequirement::from(&record),
                &record.port_id,
                None,
                &context,
            )
            .unwrap()
            .pop()
            .unwrap();

        provider
            .provision(&route, &context)
            .unwrap()
            .apply(&record, &context)
            .unwrap();

        assert_eq!(
            *control.calls.lock().unwrap(),
            vec!["add:browser.main:browser.firefox@1"]
        );
    }

    #[test]
    fn adapter_control_payload_kind_must_match_record_operation() {
        let objects = MemoryObjectStore::default();
        let provider = AdapterControlActuatorProvider::new(
            "ato.adapter.control@1",
            Arc::new(TestControl::default()),
        )
        .unwrap();
        let record = control_record(
            &objects,
            ADAPTER_REMOVE_OPERATION,
            AdapterControlPayload::Add {
                instance_id: "browser.main".to_owned(),
                implementation_id: "browser.firefox@1".to_owned(),
                config: Value::Null,
            },
        );

        assert!(matches!(
            provider.validate_payload(
                &record,
                &AdapterContext {
                    workspace: Path::new("."),
                    objects: &objects,
                }
            ),
            Err(AdapterError::InvalidPayload(_))
        ));
    }
}
