use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EXECUTION_CONTRACT_V1_SCHEMA: &str = "ato.execution-contract/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DigestAlgorithm {
    Blake3,
    Sha256,
}

impl DigestAlgorithm {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContentDigest {
    algorithm: DigestAlgorithm,
    bytes: [u8; 32],
}

impl ContentDigest {
    pub fn new(algorithm: DigestAlgorithm, bytes: [u8; 32]) -> Self {
        Self { algorithm, bytes }
    }

    pub fn algorithm(self) -> DigestAlgorithm {
        self.algorithm
    }

    pub fn bytes(self) -> [u8; 32] {
        self.bytes
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            self.algorithm.as_str(),
            hex::encode(self.bytes)
        )
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = ExecutionContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let (algorithm, encoded) = value
            .split_once(':')
            .ok_or(ExecutionContractError::InvalidContentDigest)?;
        let algorithm = match algorithm {
            "blake3" => DigestAlgorithm::Blake3,
            "sha256" => DigestAlgorithm::Sha256,
            _ => return Err(ExecutionContractError::InvalidContentDigest),
        };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionContractError::InvalidContentDigest);
        }
        let decoded =
            hex::decode(encoded).map_err(|_| ExecutionContractError::InvalidContentDigest)?;
        let bytes = decoded
            .try_into()
            .map_err(|_| ExecutionContractError::InvalidContentDigest)?;
        Ok(Self { algorithm, bytes })
    }
}

impl From<ContentDigest> for String {
    fn from(value: ContentDigest) -> Self {
        value.to_string()
    }
}

impl Serialize for ContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExecutionId(String);

impl ExecutionId {
    pub fn new(value: String) -> Result<Self, ExecutionContractError> {
        let Some(hex) = value.strip_prefix("blake3:") else {
            return Err(ExecutionContractError::InvalidExecutionId);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionContractError::InvalidExecutionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ExecutionId {
    type Error = ExecutionContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ExecutionId> for String {
    fn from(value: ExecutionId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutionContractError {
    #[error("execution contract schema must be ato.execution-contract/v1")]
    InvalidSchema,
    #[error("execution contract field '{0}' must be resolved and non-empty")]
    UnresolvedField(&'static str),
    #[error("execution contract list '{0}' must be sorted and contain no duplicates")]
    NonCanonicalList(&'static str),
    #[error("execution_id must be blake3:<64 lowercase hex characters>")]
    InvalidExecutionId,
    #[error("content digest must use blake3 or sha256 with exactly 64 lowercase hex characters")]
    InvalidContentDigest,
    #[error("failed to canonicalize execution contract: {0}")]
    Canonicalization(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContractV1 {
    pub schema: String,
    pub source: ResolvedSourceContract,
    pub target: ResolvedTargetContract,
    pub runtime: ResolvedArtifactContract,
    pub dependencies: Vec<ResolvedDependencyContract>,
    pub build_outputs: Vec<ResolvedBuildOutputContract>,
    pub launch: ResolvedLaunchContract,
    pub filesystem: ResolvedFilesystemContract,
    pub policy: ResolvedPolicyContract,
    pub guest_surface: GuestSurfaceContract,
    pub external_state: Vec<ExternalStateContract>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSourceContract {
    pub kind: String,
    pub immutable_ref: String,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTargetContract {
    pub os: String,
    pub architecture: String,
    pub abi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libc: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub observable_features: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifactContract {
    pub kind: String,
    pub resolved_ref: String,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedDependencyContract {
    pub name: String,
    pub derivation_digest: ContentDigest,
    pub output_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBuildOutputContract {
    pub name: String,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedLaunchContract {
    pub argv: Vec<String>,
    pub cwd: String,
    pub environment: Vec<EnvironmentVariableContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_bindings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVariableContract {
    pub name: String,
    pub value_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFilesystemContract {
    pub view_digest: ContentDigest,
    pub readonly_layers: Vec<ContentDigest>,
    pub writable_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPolicyContract {
    pub network_digest: ContentDigest,
    pub capability_digest: ContentDigest,
    pub filesystem_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSurfaceContract {
    pub protocol: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStateContract {
    pub name: String,
    pub target: String,
    pub access: ExternalStateAccess,
    pub schema: String,
    pub snapshot: SnapshotExclusion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalStateAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotExclusion {
    Exclude,
}

impl ExecutionContractV1 {
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        if self.schema != EXECUTION_CONTRACT_V1_SCHEMA {
            return Err(ExecutionContractError::InvalidSchema);
        }

        for (field, value) in [
            ("source.kind", self.source.kind.as_str()),
            ("source.immutable_ref", self.source.immutable_ref.as_str()),
            ("target.os", self.target.os.as_str()),
            ("target.architecture", self.target.architecture.as_str()),
            ("target.abi", self.target.abi.as_str()),
            ("runtime.kind", self.runtime.kind.as_str()),
            ("runtime.resolved_ref", self.runtime.resolved_ref.as_str()),
            ("launch.cwd", self.launch.cwd.as_str()),
            (
                "guest_surface.protocol",
                self.guest_surface.protocol.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(ExecutionContractError::UnresolvedField(field));
            }
        }
        if self.launch.argv.is_empty() || self.launch.argv.iter().any(|value| value.is_empty()) {
            return Err(ExecutionContractError::UnresolvedField("launch.argv"));
        }

        validate_named_list(
            "dependencies",
            self.dependencies.iter().map(|item| item.name.as_str()),
        )?;
        validate_named_list(
            "build_outputs",
            self.build_outputs.iter().map(|item| item.name.as_str()),
        )?;
        validate_named_list(
            "launch.environment",
            self.launch
                .environment
                .iter()
                .map(|item| item.name.as_str()),
        )?;
        validate_sorted_strings("launch.secret_bindings", &self.launch.secret_bindings)?;
        validate_sorted_digests(
            "filesystem.readonly_layers",
            &self.filesystem.readonly_layers,
        )?;
        validate_sorted_strings("filesystem.writable_paths", &self.filesystem.writable_paths)?;
        validate_sorted_strings("guest_surface.features", &self.guest_surface.features)?;
        validate_named_list(
            "external_state",
            self.external_state.iter().map(|item| item.name.as_str()),
        )?;

        for dependency in &self.dependencies {
            ensure_values("dependencies", [&dependency.name])?;
        }
        for output in &self.build_outputs {
            ensure_values("build_outputs", [&output.name])?;
        }
        for variable in &self.launch.environment {
            ensure_values("launch.environment", [&variable.name])?;
        }
        for state in &self.external_state {
            ensure_values(
                "external_state",
                [&state.name, &state.target, &state.schema],
            )?;
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ExecutionContractError> {
        self.validate()?;
        serde_jcs::to_vec(self)
            .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))
    }

    pub fn compute_execution_id(&self) -> Result<ExecutionId, ExecutionContractError> {
        let canonical = self.canonical_bytes()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(EXECUTION_CONTRACT_V1_SCHEMA.as_bytes());
        hasher.update(&[0]);
        hasher.update(&canonical);
        ExecutionId::new(format!("blake3:{}", hasher.finalize().to_hex()))
    }
}

fn validate_sorted_digests(
    field: &'static str,
    values: &[ContentDigest],
) -> Result<(), ExecutionContractError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_named_list<'a>(
    field: &'static str,
    values: impl Iterator<Item = &'a str>,
) -> Result<(), ExecutionContractError> {
    let values = values.collect::<Vec<_>>();
    if values.iter().any(|value| value.is_empty())
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_sorted_strings(
    field: &'static str,
    values: &[String],
) -> Result<(), ExecutionContractError> {
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() != values.len()
        || values.iter().any(|value| value.is_empty())
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn ensure_values<const N: usize>(
    field: &'static str,
    values: [&String; N],
) -> Result<(), ExecutionContractError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(ExecutionContractError::UnresolvedField(field));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
        ContentDigest::new(algorithm, [byte; 32])
    }

    fn sample_contract() -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                kind: "git".to_string(),
                immutable_ref: "https://example.invalid/repo@012345".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 1),
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc-2.39".to_string()),
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                resolved_ref: "node@22.14.0".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 2),
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: digest(DigestAlgorithm::Blake3, 3),
                output_digest: digest(DigestAlgorithm::Blake3, 4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: digest(DigestAlgorithm::Blake3, 5),
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "dist/server.js".to_string()],
                cwd: "/workspace".to_string(),
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: digest(DigestAlgorithm::Blake3, 6),
                }],
                secret_bindings: vec!["API_TOKEN".to_string()],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: digest(DigestAlgorithm::Blake3, 7),
                readonly_layers: vec![digest(DigestAlgorithm::Blake3, 8)],
                writable_paths: vec!["/tmp".to_string()],
            },
            policy: ResolvedPolicyContract {
                network_digest: digest(DigestAlgorithm::Blake3, 9),
                capability_digest: digest(DigestAlgorithm::Blake3, 10),
                filesystem_digest: digest(DigestAlgorithm::Blake3, 11),
            },
            guest_surface: GuestSurfaceContract {
                protocol: "ato-guest/v1".to_string(),
                features: vec!["bindings".to_string(), "exec".to_string()],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: "/data".to_string(),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
    }

    #[test]
    fn execution_id_is_domain_separated_jcs_blake3() {
        let contract = sample_contract();
        let canonical = serde_jcs::to_vec(&contract).expect("canonical contract");
        let mut expected_input = EXECUTION_CONTRACT_V1_SCHEMA.as_bytes().to_vec();
        expected_input.push(0);
        expected_input.extend(canonical);

        assert_eq!(
            contract.compute_execution_id().expect("execution id"),
            ExecutionId::new(format!("blake3:{}", blake3::hash(&expected_input).to_hex()))
                .expect("valid id")
        );
    }

    #[test]
    fn content_digest_rejects_placeholders_wrong_lengths_and_uppercase_hex() {
        for invalid in [
            "latest",
            "sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:unknown",
            "sha256:aa",
            "blake3:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                ContentDigest::try_from(invalid.to_string()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn content_digest_round_trips_as_a_canonical_string() {
        let expected = digest(DigestAlgorithm::Sha256, 0xab);
        let json = serde_json::to_string(&expected).unwrap();
        assert_eq!(json, format!("\"sha256:{}\"", "ab".repeat(32)));
        assert_eq!(
            serde_json::from_str::<ContentDigest>(&json).unwrap(),
            expected
        );
    }

    #[test]
    fn resolved_target_architecture_changes_execution_id() {
        let x86 = sample_contract();
        let mut arm = x86.clone();
        arm.target.architecture = "aarch64".to_string();

        assert_ne!(
            x86.compute_execution_id().unwrap(),
            arm.compute_execution_id().unwrap()
        );
    }

    #[test]
    fn external_state_contract_changes_execution_id() {
        let first = sample_contract();
        let mut second = first.clone();
        second.external_state[0].schema = "2".to_string();

        assert_ne!(
            first.compute_execution_id().unwrap(),
            second.compute_execution_id().unwrap()
        );
    }

    #[test]
    fn unknown_identity_field_fails_closed() {
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("runner".to_string(), serde_json::json!("local"));

        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn malformed_execution_id_fails_deserialization() {
        assert!(serde_json::from_str::<ExecutionId>("\"blake3:not-a-digest\"").is_err());
    }

    #[test]
    fn uppercase_execution_id_is_rejected_as_noncanonical() {
        assert!(ExecutionId::new(format!("blake3:{}", "A".repeat(64))).is_err());
    }

    #[test]
    fn execution_contract_and_id_are_authenticated_by_lock_id() {
        let contract = sample_contract();
        let execution_id = contract.compute_execution_id().unwrap();
        let lock = crate::ato_lock::AtoLock {
            execution_contract: Some(contract),
            execution_id: Some(execution_id),
            ..crate::ato_lock::AtoLock::default()
        };
        let baseline = crate::ato_lock::compute_lock_id(&lock).unwrap();

        let mut contract_mutated = lock.clone();
        contract_mutated
            .execution_contract
            .as_mut()
            .unwrap()
            .target
            .architecture = "aarch64".to_string();
        assert_ne!(
            baseline,
            crate::ato_lock::compute_lock_id(&contract_mutated).unwrap()
        );

        let mut id_mutated = lock;
        id_mutated.execution_id =
            Some(ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap());
        assert_ne!(
            baseline,
            crate::ato_lock::compute_lock_id(&id_mutated).unwrap()
        );
    }

    #[test]
    fn unresolved_or_empty_identity_fields_fail_closed() {
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value["runtime"]["digest"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn lock_metadata_does_not_change_execution_id() {
        let contract = sample_contract();
        let expected = contract.compute_execution_id().unwrap();
        let mut lock = crate::ato_lock::AtoLock {
            execution_contract: Some(contract),
            execution_id: Some(expected.clone()),
            ..crate::ato_lock::AtoLock::default()
        };

        lock.generated_at = Some("2026-07-21T00:00:00Z".to_string());
        lock.attestations
            .entries
            .insert("builder".to_string(), serde_json::json!("runner-a"));

        assert_eq!(
            lock.execution_contract
                .as_ref()
                .unwrap()
                .compute_execution_id()
                .unwrap(),
            expected
        );
    }

    #[test]
    fn lock_validation_rejects_execution_id_mismatch() {
        let contract = sample_contract();
        let mut lock = crate::ato_lock::AtoLock {
            execution_contract: Some(contract),
            execution_id: Some(ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap()),
            ..crate::ato_lock::AtoLock::default()
        };
        crate::ato_lock::recompute_lock_id(&mut lock).unwrap();

        let errors = crate::ato_lock::validate_persisted_strict(&lock).unwrap_err();
        assert!(errors.iter().any(|error| matches!(
            error,
            crate::ato_lock::AtoLockValidationError::ExecutionIdMismatch { .. }
        )));
    }
}
