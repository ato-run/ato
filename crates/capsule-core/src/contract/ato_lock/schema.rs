use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const ATO_LOCK_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtoLock {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock_id: Option<LockId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub features: LockFeatures,
    #[serde(default)]
    pub resolution: ResolutionSection,
    #[serde(default)]
    pub contract: ContractSection,
    #[serde(default)]
    pub binding: BindingSection,
    #[serde(default)]
    pub policy: PolicySection,
    #[serde(default)]
    pub attestations: AttestationsSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<LockSignature>,
}

impl Default for AtoLock {
    fn default() -> Self {
        Self {
            schema_version: ATO_LOCK_SCHEMA_VERSION,
            lock_id: None,
            generated_at: None,
            features: LockFeatures::default(),
            resolution: ResolutionSection::default(),
            contract: ContractSection::default(),
            binding: BindingSection::default(),
            policy: PolicySection::default(),
            attestations: AttestationsSection::default(),
            signatures: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LockId(String);

impl LockId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate_format(&self) -> Result<(), String> {
        let Some(digest) = self.0.strip_prefix("blake3:") else {
            return Err(format!(
                "lock_id must start with 'blake3:', got '{}'",
                self.0
            ));
        };

        if digest.len() != 64 || !digest.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(format!(
                "lock_id has invalid blake3 hex digest: '{}'",
                self.0
            ));
        }

        Ok(())
    }

    pub fn algorithm(&self) -> Option<&str> {
        self.0.split_once(':').map(|(algorithm, _)| algorithm)
    }

    pub fn digest_hex(&self) -> Option<&str> {
        self.0.split_once(':').map(|(_, digest)| digest)
    }
}

impl From<String> for LockId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for LockId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LockFeatures {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared: Vec<FeatureName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_for_execution: Vec<FeatureName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub implementation_phase: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeatureName {
    Known(KnownFeature),
    Unknown(String),
}

impl FeatureName {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Known(feature) => feature.as_str(),
            Self::Unknown(value) => value.as_str(),
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

impl Serialize for FeatureName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FeatureName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.parse::<KnownFeature>() {
            Ok(feature) => Self::Known(feature),
            Err(_) => Self::Unknown(value),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownFeature {
    ReadOnlyRootFs,
    Identity,
    ReservedEnvPrefixes,
    RequiredSupervisor,
    EnforcedNetwork,
}

impl KnownFeature {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnlyRootFs => "read_only_root_fs",
            Self::Identity => "identity",
            Self::ReservedEnvPrefixes => "reserved_env_prefixes",
            Self::RequiredSupervisor => "required_supervisor",
            Self::EnforcedNetwork => "enforced_network",
        }
    }
}

impl fmt::Display for KnownFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for KnownFeature {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read_only_root_fs" => Ok(Self::ReadOnlyRootFs),
            "identity" => Ok(Self::Identity),
            "reserved_env_prefixes" => Ok(Self::ReservedEnvPrefixes),
            "required_supervisor" => Ok(Self::RequiredSupervisor),
            "enforced_network" => Ok(Self::EnforcedNetwork),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ResolutionSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedValue>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContractSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedValue>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryEnvironment {
    pub strategy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<DeliveryService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap: Option<DeliveryBootstrap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<DeliveryRepair>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryService {
    pub name: String,
    pub from: String,
    pub lifecycle: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<DeliveryHealthcheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryHealthcheck {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryBootstrap {
    #[serde(default)]
    pub requires_personalization: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_tiers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DeliveryRepair {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

pub fn parse_delivery_environment_value(value: &Value) -> Result<DeliveryEnvironment, String> {
    serde_json::from_value::<DeliveryEnvironment>(value.clone())
        .map_err(|err| format!("contract.delivery.install.environment is invalid: {err}"))
}

pub fn delivery_environment(lock: &AtoLock) -> Result<Option<DeliveryEnvironment>, String> {
    let Some(delivery) = lock.contract.entries.get("delivery") else {
        return Ok(None);
    };
    let Some(install) = delivery.get("install").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(environment) = install.get("environment") else {
        return Ok(None);
    };
    parse_delivery_environment_value(environment).map(Some)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BindingSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedValue>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PolicySection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedValue>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AttestationsSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedValue>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LockSignature {
    pub kind: String,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payload: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UnresolvedValue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub reason: UnresolvedReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UnresolvedReason {
    #[default]
    InsufficientEvidence,
    Ambiguity,
    DeferredHostLocalBinding,
    PolicyGatedResolution,
    ExplicitSelectionRequired,
    Unknown(String),
}

impl UnresolvedReason {
    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            Self::InsufficientEvidence => Cow::Borrowed("insufficient_evidence"),
            Self::Ambiguity => Cow::Borrowed("ambiguity"),
            Self::DeferredHostLocalBinding => Cow::Borrowed("deferred_host_local_binding"),
            Self::PolicyGatedResolution => Cow::Borrowed("policy_gated_resolution"),
            Self::ExplicitSelectionRequired => Cow::Borrowed("explicit_selection_required"),
            Self::Unknown(value) => Cow::Borrowed(value.as_str()),
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

impl fmt::Display for UnresolvedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str().as_ref())
    }
}

impl FromStr for UnresolvedReason {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "insufficient_evidence" => Self::InsufficientEvidence,
            "ambiguity" => Self::Ambiguity,
            "deferred_host_local_binding" => Self::DeferredHostLocalBinding,
            "policy_gated_resolution" => Self::PolicyGatedResolution,
            "explicit_selection_required" => Self::ExplicitSelectionRequired,
            other => Self::Unknown(other.to_string()),
        })
    }
}

impl Serialize for UnresolvedReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str().as_ref())
    }
}

impl<'de> Deserialize<'de> for UnresolvedReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}
