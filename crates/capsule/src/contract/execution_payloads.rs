//! The opaque sub-contract payloads of `ato.execution-contract/v1`.
//!
//! Each [`OpaqueContractDigestV1`] on the contract commits
//! `blake3(UTF8(domain) || 0x00 || JCS(payload))`. v1 froze the domains and the
//! rule; the payload SCHEMAS were deferred. These are them.
//!
//! # What a payload may contain
//!
//! Only facts the tree can actually state, per ADR-015's taxonomy. Three rules
//! decided every field below:
//!
//! 1. **A fixture value is not a normative default.** Where the codebase has no
//!    concept for something, the payload does not invent one — it omits the
//!    field and this doc says why.
//! 2. **A convention IS statable, but only explicitly.** "The init always mounts
//!    `/tmp` as tmpfs" is a fact about the built image, and a payload that
//!    commits it makes changing it identity-bearing. That is a decision, so it
//!    is written down here rather than left implicit.
//! 3. **Absent means absent.** Every collection follows the contract's own
//!    empty-collection rule: omitted when empty, never `[]` (ADR-015 §6.3).
//!
//! # Why these are typed structs, not `serde_json::Value`
//!
//! The digest is over the JCS of whatever is passed. A free-form `Value` lets
//! two producers spell the same facts differently — an extra key, a different
//! number type — and derive different digests for identical executions. A typed
//! struct with `deny_unknown_fields` and a pinned `schema` makes the spelling
//! single-valued, the same way [`EnvironmentValuePayloadV1`] already does for a
//! single value.
//!
//! [`EnvironmentValuePayloadV1`]: crate::execution_contract::EnvironmentValuePayloadV1

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::execution_contract::{ExecutionContractError, OpaqueContractDomainV1};

/// Every payload pins its own schema string to its domain, so a payload is
/// self-describing under the identity its digest commits.
macro_rules! schema_const {
    ($name:ident, $domain:expr) => {
        pub const $name: &str = $domain.as_str();
    };
}

schema_const!(
    SOURCE_PROJECTION_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::SourceProjection
);
schema_const!(
    RUNTIME_DYNAMIC_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::RuntimeDynamic
);
schema_const!(
    PROCESS_MODEL_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::ProcessModel
);
schema_const!(
    ENVIRONMENT_POLICY_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::EnvironmentPolicy
);
schema_const!(
    FILESYSTEM_TOPOLOGY_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::FilesystemTopology
);
schema_const!(
    NETWORK_POLICY_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::NetworkPolicy
);
schema_const!(
    CAPABILITY_POLICY_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::CapabilityPolicy
);
schema_const!(
    FILESYSTEM_POLICY_PAYLOAD_V1_SCHEMA,
    OpaqueContractDomainV1::FilesystemPolicy
);

fn invalid(reason: &'static str) -> ExecutionContractError {
    ExecutionContractError::InvalidEnvironmentValuePayload(reason)
}

fn require_schema(actual: &str, expected: &str) -> Result<(), ExecutionContractError> {
    if actual == expected {
        Ok(())
    } else {
        Err(invalid("payload schema does not match its domain"))
    }
}

/// A sorted, duplicate-free string list. Every set-like collection in a payload
/// uses it, so two producers with different iteration orders cannot derive
/// different digests for the same set.
fn require_sorted_unique(values: &[String]) -> Result<(), ExecutionContractError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid(
            "set-like payload collections must be sorted and duplicate-free",
        ));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// source.projection_digest
// ─────────────────────────────────────────────────────────────────────────────

/// How the source tree was projected before it was hashed.
///
/// The rules themselves are frozen by the named profile — that is what a
/// versioned profile IS — so this commits the profile identity plus the one
/// thing that genuinely varies per capsule: which control files were held out.
///
/// The excluded set is per-capsule because the canonical lock has two admissible
/// names, and a repository carrying `ato.lock.json` had a different file
/// withheld from its digest than one carrying `capsule.lock`. Committing the
/// profile alone would make those two indistinguishable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProjectionPayloadV1 {
    pub schema: String,
    /// The frozen admissibility + hashing profile (A1v2 rules 1-8).
    pub profile: String,
    /// Repository-relative paths withheld from the projection, sorted.
    pub excluded_control_files: Vec<String>,
}

/// The A1v2 source-tree profile: NFC required (not applied), all symlinks
/// rejected, case-fold collisions rejected within a directory, submodule and
/// LFS signals rejected, only the owner-execute bit folded into identity, and
/// the file/size caps.
pub const SOURCE_TREE_PROFILE_A1V2: &str = "ato.source-tree/a1v2";

impl SourceProjectionPayloadV1 {
    pub fn a1v2(excluded_control_files: Vec<String>) -> Self {
        let mut excluded = excluded_control_files;
        excluded.sort();
        excluded.dedup();
        Self {
            schema: SOURCE_PROJECTION_PAYLOAD_V1_SCHEMA.to_string(),
            profile: SOURCE_TREE_PROFILE_A1V2.to_string(),
            excluded_control_files: excluded,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, SOURCE_PROJECTION_PAYLOAD_V1_SCHEMA)?;
        if self.profile != SOURCE_TREE_PROFILE_A1V2 {
            return Err(invalid("unknown source-tree profile"));
        }
        if self.excluded_control_files.is_empty() {
            // The manifest is ALWAYS withheld, so an empty set means the
            // producer did not record what it did.
            return Err(invalid(
                "the projection always withholds at least the manifest",
            ));
        }
        require_sorted_unique(&self.excluded_control_files)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// runtime.dynamic_contract_digest
// ─────────────────────────────────────────────────────────────────────────────

/// Launch-time runtime behaviour beyond the artifact bytes.
///
/// `invocation_prefix` is the part of the resolved argv the RUNTIME contributes
/// — an interpreter, its flags — as distinct from the author's command. It is
/// identity-bearing for the obvious reason: the same program under
/// `python -O` is a different execution than under `python`.
///
/// Deliberately absent: a plugin/loader surface and JIT switches. The tree has
/// no concept for either, and inventing a field that every producer would fill
/// with the same empty value adds nothing while pretending to constrain
/// something (ADR-015 §2, rule 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDynamicPayloadV1 {
    pub schema: String,
    /// The resolved runtime family, e.g. `source`, `oci`, `wasm`.
    pub kind: String,
    /// argv elements the runtime prepends to the authored command. Omitted when
    /// the command is executed directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invocation_prefix: Vec<String>,
}

impl RuntimeDynamicPayloadV1 {
    pub fn new(kind: impl Into<String>, invocation_prefix: Vec<String>) -> Self {
        Self {
            schema: RUNTIME_DYNAMIC_PAYLOAD_V1_SCHEMA.to_string(),
            kind: kind.into(),
            invocation_prefix,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, RUNTIME_DYNAMIC_PAYLOAD_V1_SCHEMA)?;
        if self.kind.trim().is_empty() {
            return Err(invalid("runtime kind must not be empty"));
        }
        if self.invocation_prefix.iter().any(|a| a.contains('\0')) {
            return Err(invalid("an argv element must not contain a NUL"));
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// launch.process_model_digest
// ─────────────────────────────────────────────────────────────────────────────

/// The process structure the workload runs under.
///
/// `pid1` records what actually holds PID 1, which on the current image is the
/// init SCRIPT — the guest agent is backgrounded from it, not exec'd as PID 1.
/// Writing down what is true rather than what the doc comments say is the whole
/// value of committing this facet: the day the agent does become PID 1, the
/// identity moves, which is correct.
///
/// Deliberately absent: restart policy, reaping and stop/kill signals. No field
/// for any of them exists on the snapshot path, so a payload that named them
/// would be committing a value no author chose and no code reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessModelPayloadV1 {
    pub schema: String,
    /// `single` — one workload process; `supervised` — a guest agent starts and
    /// gates several services.
    pub structure: ProcessStructureV1,
    /// What holds PID 1 in the guest.
    pub pid1: Pid1RoleV1,
    /// Supervised service names, sorted. Omitted for a single-process capsule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessStructureV1 {
    Single,
    Supervised,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pid1RoleV1 {
    /// The generated init script is PID 1 and starts the workload.
    InitScript,
}

impl ProcessModelPayloadV1 {
    pub fn single_process() -> Self {
        Self {
            schema: PROCESS_MODEL_PAYLOAD_V1_SCHEMA.to_string(),
            structure: ProcessStructureV1::Single,
            pid1: Pid1RoleV1::InitScript,
            services: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, PROCESS_MODEL_PAYLOAD_V1_SCHEMA)?;
        match self.structure {
            ProcessStructureV1::Single if !self.services.is_empty() => {
                Err(invalid("a single-process model declares no services"))
            }
            ProcessStructureV1::Supervised if self.services.is_empty() => {
                Err(invalid("a supervised model declares at least one service"))
            }
            _ => require_sorted_unique(&self.services),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// launch.environment_policy_digest
// ─────────────────────────────────────────────────────────────────────────────

/// Which variables must be supplied, and what the guest inherits.
///
/// `inheritance` is the field that matters most and the one most likely to be
/// assumed wrong: on the snapshot path the guest environment is exactly what the
/// image was built with, with NO host passthrough — unlike the CLI executor,
/// which passes an allowlist through. Committing it means a capsule cannot
/// silently move between those two worlds without moving its identity.
///
/// Deliberately absent: value normalization. No rule exists anywhere in the tree
/// (no case folding, no trimming, no encoding beyond the per-value payload's
/// `utf8`), so naming one here would invent it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentPolicyPayloadV1 {
    pub schema: String,
    pub inheritance: EnvInheritanceV1,
    /// Names that MUST be supplied at launch, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Names that MAY be supplied at launch, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvInheritanceV1 {
    /// The guest sees only what the image and its bindings provide.
    None,
    /// A fixed host allowlist is passed through (the CLI executor's behaviour).
    HostAllowlist,
}

impl EnvironmentPolicyPayloadV1 {
    pub fn sealed_guest(mut required: Vec<String>, mut optional: Vec<String>) -> Self {
        required.sort();
        required.dedup();
        optional.sort();
        optional.dedup();
        Self {
            schema: ENVIRONMENT_POLICY_PAYLOAD_V1_SCHEMA.to_string(),
            inheritance: EnvInheritanceV1::None,
            required,
            optional,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, ENVIRONMENT_POLICY_PAYLOAD_V1_SCHEMA)?;
        require_sorted_unique(&self.required)?;
        require_sorted_unique(&self.optional)?;
        if self
            .required
            .iter()
            .any(|name| self.optional.binary_search(name).is_ok())
        {
            return Err(invalid("a variable is required or optional, never both"));
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// filesystem.topology_digest
// ─────────────────────────────────────────────────────────────────────────────

/// What is mounted where, and how.
///
/// Ordered, because mount order is observable: a later mount over an earlier
/// path hides it. The contract's `view_digest` commits the CONTENT of the
/// composed view; this commits its STRUCTURE, so a topology change is
/// identity-bearing even when the mounted bytes are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemTopologyPayloadV1 {
    pub schema: String,
    /// In mount order.
    pub mounts: Vec<MountV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountV1 {
    /// Absolute guest path.
    pub target: String,
    pub kind: MountKindV1,
    pub access: MountAccessV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountKindV1 {
    /// The composed immutable image.
    RootImage,
    Tmpfs,
    Proc,
    Sysfs,
    Devtmpfs,
    /// A durable per-owner volume.
    StateVolume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountAccessV1 {
    ReadOnly,
    ReadWrite,
    /// Kernel-managed pseudo-filesystems, where the distinction does not apply.
    Kernel,
}

impl FilesystemTopologyPayloadV1 {
    /// The topology the current builder produces for a capsule with no durable
    /// volumes — read straight off the generated init script, not assumed.
    pub fn sealed_no_volumes() -> Self {
        let mount = |target: &str, kind, access| MountV1 {
            target: target.to_string(),
            kind,
            access,
        };
        Self {
            schema: FILESYSTEM_TOPOLOGY_PAYLOAD_V1_SCHEMA.to_string(),
            mounts: vec![
                mount("/", MountKindV1::RootImage, MountAccessV1::ReadOnly),
                mount("/proc", MountKindV1::Proc, MountAccessV1::Kernel),
                mount("/sys", MountKindV1::Sysfs, MountAccessV1::Kernel),
                mount("/dev", MountKindV1::Devtmpfs, MountAccessV1::Kernel),
                mount("/tmp", MountKindV1::Tmpfs, MountAccessV1::ReadWrite),
                mount("/run", MountKindV1::Tmpfs, MountAccessV1::ReadWrite),
                mount("/var/tmp", MountKindV1::Tmpfs, MountAccessV1::ReadWrite),
            ],
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, FILESYSTEM_TOPOLOGY_PAYLOAD_V1_SCHEMA)?;
        if self.mounts.is_empty() {
            return Err(invalid("a topology has at least the root mount"));
        }
        if self.mounts[0].kind != MountKindV1::RootImage {
            return Err(invalid("the first mount is the root image"));
        }
        let mut seen = BTreeMap::new();
        for mount in &self.mounts {
            if !mount.target.starts_with('/') || mount.target.contains("/../") {
                return Err(invalid("a mount target must be an absolute guest path"));
            }
            if seen.insert(mount.target.clone(), ()).is_some() {
                return Err(invalid("two mounts claim one target"));
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// policy.network_digest
// ─────────────────────────────────────────────────────────────────────────────

/// The effective network policy.
///
/// Deliberately absent: DNS policy and an isolation mode. The tree has no field
/// for either — the only DNS-adjacent data is in-guest service aliases, which a
/// single-process capsule does not have — so ADR-015 §4.3's option to EXCLUDE
/// them is taken, and taken visibly. Adding either later is a payload schema
/// version, not a silent widening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPolicyPayloadV1 {
    pub schema: String,
    /// Hosts the workload may reach, sorted. Omitted when it may reach none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_allow: Vec<String>,
    /// Guest ports reachable from outside, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ingress_ports: Vec<u16>,
    /// Whether an unlisted egress is denied. Always true today; committed
    /// because a future permissive mode must not be identity-silent.
    pub egress_fail_closed: bool,
}

impl NetworkPolicyPayloadV1 {
    pub fn new(mut egress_allow: Vec<String>, mut ingress_ports: Vec<u16>) -> Self {
        egress_allow.sort();
        egress_allow.dedup();
        ingress_ports.sort_unstable();
        ingress_ports.dedup();
        Self {
            schema: NETWORK_POLICY_PAYLOAD_V1_SCHEMA.to_string(),
            egress_allow,
            ingress_ports,
            egress_fail_closed: true,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, NETWORK_POLICY_PAYLOAD_V1_SCHEMA)?;
        require_sorted_unique(&self.egress_allow)?;
        if self.ingress_ports.windows(2).any(|p| p[0] >= p[1]) {
            return Err(invalid("ingress ports must be sorted and duplicate-free"));
        }
        if self.ingress_ports.contains(&0) {
            return Err(invalid("port 0 is never a declared surface"));
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// policy.capability_digest
// ─────────────────────────────────────────────────────────────────────────────

/// The declared capability posture.
///
/// Every field is optional, and that is normative rather than convenient: the
/// security schema's own rule is *"absence means 'not declared' and must not be
/// treated as any particular level."* Collapsing an undeclared posture to a
/// default here would make two capsules — one that declared `network = "none"`
/// and one that declared nothing — derive the same identity while making
/// different claims.
///
/// Deliberately absent: device access, seccomp profile and Linux capability
/// sets. No field for any of them exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicyPayloadV1 {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_writes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_required: Option<bool>,
}

impl CapabilityPolicyPayloadV1 {
    /// Nothing declared — distinct from every declared level.
    pub fn undeclared() -> Self {
        Self {
            schema: CAPABILITY_POLICY_PAYLOAD_V1_SCHEMA.to_string(),
            ..Default::default()
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, CAPABILITY_POLICY_PAYLOAD_V1_SCHEMA)?;
        for value in [&self.network, &self.fs_writes, &self.side_effects]
            .into_iter()
            .flatten()
        {
            if value.trim().is_empty() {
                return Err(invalid(
                    "a declared capability level must not be an empty string",
                ));
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// policy.filesystem_digest
// ─────────────────────────────────────────────────────────────────────────────

/// What the process may read and write, as opposed to what is mounted.
///
/// The distinction from `filesystem.topology_digest` is deliberate and was a
/// naming hazard worth resolving: topology is STRUCTURE (what is mounted where),
/// this is CAPABILITY (what the process may do). A capsule can have one mount
/// and several policy paths, or the reverse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPolicyPayloadV1 {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_only: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_write: Vec<String>,
}

impl FilesystemPolicyPayloadV1 {
    pub fn new(mut read_only: Vec<String>, mut read_write: Vec<String>) -> Self {
        for list in [&mut read_only, &mut read_write] {
            list.sort();
            list.dedup();
        }
        Self {
            schema: FILESYSTEM_POLICY_PAYLOAD_V1_SCHEMA.to_string(),
            read_only,
            read_write,
        }
    }

    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        require_schema(&self.schema, FILESYSTEM_POLICY_PAYLOAD_V1_SCHEMA)?;
        require_sorted_unique(&self.read_only)?;
        require_sorted_unique(&self.read_write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_contract::opaque_subcontract_digest;

    /// Every payload's `schema` IS its domain string, so a payload is
    /// self-describing under the identity its digest commits — and a payload
    /// placed under the wrong field is rejected by its own schema check before
    /// the domain check even runs.
    #[test]
    fn every_payload_pins_its_schema_to_its_domain() {
        for (schema, domain) in [
            (
                SOURCE_PROJECTION_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::SourceProjection,
            ),
            (
                RUNTIME_DYNAMIC_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::RuntimeDynamic,
            ),
            (
                PROCESS_MODEL_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::ProcessModel,
            ),
            (
                ENVIRONMENT_POLICY_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::EnvironmentPolicy,
            ),
            (
                FILESYSTEM_TOPOLOGY_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::FilesystemTopology,
            ),
            (
                NETWORK_POLICY_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::NetworkPolicy,
            ),
            (
                CAPABILITY_POLICY_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::CapabilityPolicy,
            ),
            (
                FILESYSTEM_POLICY_PAYLOAD_V1_SCHEMA,
                OpaqueContractDomainV1::FilesystemPolicy,
            ),
        ] {
            assert_eq!(schema, domain.as_str());
        }
    }

    #[test]
    fn the_constructors_produce_valid_payloads() {
        SourceProjectionPayloadV1::a1v2(vec!["capsule.toml".into(), "capsule.lock".into()])
            .validate()
            .expect("source projection");
        RuntimeDynamicPayloadV1::new("source", vec!["python3".into()])
            .validate()
            .expect("runtime dynamic");
        ProcessModelPayloadV1::single_process()
            .validate()
            .expect("process model");
        EnvironmentPolicyPayloadV1::sealed_guest(vec!["API_KEY".into()], vec![])
            .validate()
            .expect("environment policy");
        FilesystemTopologyPayloadV1::sealed_no_volumes()
            .validate()
            .expect("topology");
        NetworkPolicyPayloadV1::new(vec![], vec![8080])
            .validate()
            .expect("network");
        CapabilityPolicyPayloadV1::undeclared()
            .validate()
            .expect("capability");
        FilesystemPolicyPayloadV1::new(vec![], vec![])
            .validate()
            .expect("filesystem policy");
    }

    /// The lock's NAME is part of the projection: a repository carrying
    /// `ato.lock.json` had a different file withheld from its source digest
    /// than one carrying `capsule.lock`, and committing only the profile would
    /// make those two indistinguishable.
    #[test]
    fn the_excluded_control_files_are_identity_bearing() {
        let canonical =
            SourceProjectionPayloadV1::a1v2(vec!["capsule.toml".into(), "capsule.lock".into()]);
        let deprecated =
            SourceProjectionPayloadV1::a1v2(vec!["capsule.toml".into(), "ato.lock.json".into()]);
        assert_ne!(
            opaque_subcontract_digest(OpaqueContractDomainV1::SourceProjection, &canonical)
                .unwrap(),
            opaque_subcontract_digest(OpaqueContractDomainV1::SourceProjection, &deprecated)
                .unwrap()
        );
    }

    /// Order is not identity for a set-like list — the constructor sorts, so two
    /// producers that enumerate differently derive the same digest.
    #[test]
    fn set_like_collections_are_order_insensitive_through_their_constructor() {
        let one = EnvironmentPolicyPayloadV1::sealed_guest(
            vec!["B".into(), "A".into()],
            vec!["D".into(), "C".into()],
        );
        let two = EnvironmentPolicyPayloadV1::sealed_guest(
            vec!["A".into(), "B".into()],
            vec!["C".into(), "D".into()],
        );
        assert_eq!(one, two);
    }

    /// An unsorted list built by hand is REFUSED rather than sorted on the way
    /// in: silently normalizing it would let a producer derive a digest from
    /// bytes it never actually held.
    #[test]
    fn an_unsorted_or_duplicated_collection_is_refused_not_normalized() {
        let mut payload = EnvironmentPolicyPayloadV1::sealed_guest(vec![], vec![]);
        payload.required = vec!["B".into(), "A".into()];
        assert!(payload.validate().is_err());
        payload.required = vec!["A".into(), "A".into()];
        assert!(payload.validate().is_err());
    }

    /// Mount order is observable — a later mount over an earlier path hides it —
    /// so the topology is a LIST, and reordering it changes the identity.
    #[test]
    fn mount_order_is_identity_bearing() {
        let topology = FilesystemTopologyPayloadV1::sealed_no_volumes();
        let mut reordered = topology.clone();
        reordered.mounts.swap(4, 5);
        assert_ne!(
            opaque_subcontract_digest(OpaqueContractDomainV1::FilesystemTopology, &topology)
                .unwrap(),
            opaque_subcontract_digest(OpaqueContractDomainV1::FilesystemTopology, &reordered)
                .unwrap()
        );
    }

    /// The no-volume topology is what the builder's init script actually does,
    /// read off it rather than assumed — the root is read-only and the three
    /// tmpfs mounts are the only writable ones.
    #[test]
    fn the_sealed_no_volume_topology_matches_the_generated_init() {
        let topology = FilesystemTopologyPayloadV1::sealed_no_volumes();
        let targets: Vec<&str> = topology.mounts.iter().map(|m| m.target.as_str()).collect();
        assert_eq!(
            targets,
            ["/", "/proc", "/sys", "/dev", "/tmp", "/run", "/var/tmp"]
        );
        assert_eq!(topology.mounts[0].access, MountAccessV1::ReadOnly);
        let writable: Vec<&str> = topology
            .mounts
            .iter()
            .filter(|m| m.access == MountAccessV1::ReadWrite)
            .map(|m| m.target.as_str())
            .collect();
        assert_eq!(writable, ["/tmp", "/run", "/var/tmp"]);
    }

    /// "Not declared" is its own value, distinct from every declared level.
    ///
    /// The security schema says so normatively, and collapsing it to a default
    /// would make a capsule that declared `network = "none"` and one that
    /// declared nothing derive the same identity while making different claims.
    #[test]
    fn an_undeclared_capability_posture_is_distinct_from_a_declared_one() {
        let undeclared = CapabilityPolicyPayloadV1::undeclared();
        let declared = CapabilityPolicyPayloadV1 {
            network: Some("none".into()),
            ..CapabilityPolicyPayloadV1::undeclared()
        };
        assert_ne!(
            opaque_subcontract_digest(OpaqueContractDomainV1::CapabilityPolicy, &undeclared)
                .unwrap(),
            opaque_subcontract_digest(OpaqueContractDomainV1::CapabilityPolicy, &declared).unwrap()
        );
        // And the undeclared form carries no key at all, per the contract's
        // empty/absent rule.
        let json = serde_json::to_value(&undeclared).unwrap();
        for key in ["network", "fs_writes", "side_effects", "secrets_required"] {
            assert!(json.get(key).is_none(), "{key} must be omitted");
        }
    }

    /// A supervised model must name its services and a single-process one must
    /// not — the two are different executions and the payload cannot be silent
    /// about which it is.
    #[test]
    fn the_process_structure_and_its_services_must_agree() {
        let mut payload = ProcessModelPayloadV1::single_process();
        payload.services = vec!["app".into()];
        assert!(payload.validate().is_err(), "single declares no services");

        payload.structure = ProcessStructureV1::Supervised;
        payload.validate().expect("supervised with a service");

        payload.services.clear();
        assert!(
            payload.validate().is_err(),
            "supervised declares at least one"
        );
    }

    /// A payload whose schema names another domain is refused by its own check,
    /// before the digest is ever computed.
    #[test]
    fn a_payload_wearing_another_domains_schema_is_refused() {
        let mut payload = NetworkPolicyPayloadV1::new(vec![], vec![]);
        payload.schema = CAPABILITY_POLICY_PAYLOAD_V1_SCHEMA.to_string();
        assert!(payload.validate().is_err());
    }
}
