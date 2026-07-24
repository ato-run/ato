//! Strict v0.3 manifest input gate + normalization adapter for Capsule
//! Program Identity (ADR-014 §2).
//!
//! Two layers with strictly separated jobs (ADR-014 §2.0.1):
//!
//! * [`ProgramManifestV03Input`] is the STRUCTURAL GATE over the raw
//!   `capsule.toml` text. Its only job is *rejection*: every one of the 41
//!   `CapsuleManifest` top-level fields is explicit, `deny_unknown_fields`
//!   applies at the top level and on every nested struct of an
//!   identity-bearing section, and serde aliases mirror the real authoring
//!   types exactly (`command`→`entrypoint`, `build`→`build_command`,
//!   `install`→`install_command`, `prestart`→`prestart_command`,
//!   `run`→`run_command`, `depends_on`→`needs`). Leaf VALUES are deliberately
//!   lenient (`toml::Value`) — meaning belongs to the existing v0.3
//!   normalizer, so the gate polices key sets, never semantics.
//! * [`program_intent_from_v03`] is the ADAPTER. It runs the gate first, then
//!   builds [`ProgramManifestIntentV1`] from the POST-NORMALIZATION
//!   [`CapsuleManifest`] model (`load_manifest` output), consuming the
//!   normalizer's canonical values wherever a v0.3 normalization exists.
//!
//! `TargetsConfig` mixes known fields with arbitrary named targets via
//! `#[serde(flatten)]`, so `deny_unknown_fields` alone cannot police it —
//! [`ProgramTargetsV03Input`] carries the required custom deserializer:
//! reserved keys (`preference`, `source_digest`, `port`, `startup_timeout`,
//! `env`, `health_check`, `wasm`, `source`, `oci`) parse as known fields;
//! every other key parses as a strict named-target struct; unknown keys
//! inside a named or structured target are rejected naming the offending key
//! and target label.
//!
//! Fail-closed rules enforced here (ADR-014 §2.1/§2.2):
//! `workspace` present ⇒ [`CapsuleProgramError::UnsupportedField`];
//! `targets.<label>.engine_path` present ⇒ fail closed; `working_dir` on a
//! Wasm target ⇒ fail closed; `targets.<label>.model` must be a relative,
//! in-tree, EXISTING path under the selected root; `build.inputs.lockfiles`
//! and `targets.source.dependencies` are existence-checked, `artifacts` and
//! entrypoints stay lexical-only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::Path;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::capsule_program_contract::{
    CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA, CapsuleProgramError, CasContentDigest,
    ContainerUserSpec, GitCommitRevision, GlobPattern, HttpRequestTarget, NormalizedBindingIntent,
    NormalizedBuildInputsIntent, NormalizedBuildIntent, NormalizedBuildLifecycleIntent,
    NormalizedCapabilitiesIntent, NormalizedCliExportIntent, NormalizedCommandIntent,
    NormalizedConfigFieldIntent, NormalizedContextIntent, NormalizedContractIntent,
    NormalizedContractStateIntent, NormalizedDependencyIntent, NormalizedDependencyStateIntent,
    NormalizedEgressIdRuleIntent, NormalizedExecutionEntrypointIntent, NormalizedExecutionIntent,
    NormalizedExportsIntent, NormalizedExternalDependencyIntent, NormalizedExternalInjectionIntent,
    NormalizedExternalIntent, NormalizedFoundationRequirementsIntent,
    NormalizedGeneratedBindingIntent, NormalizedHostCapabilityIntent, NormalizedIngressIntent,
    NormalizedIngressRouteIntent, NormalizedIsolationIntent, NormalizedModelIntent,
    NormalizedNetworkIntent, NormalizedPackIntent, NormalizedParamValueIntent,
    NormalizedPlatformArtifactIntent, NormalizedPolymorphismIntent, NormalizedReadinessProbeIntent,
    NormalizedReadyProbeIntent, NormalizedRequirementsIntent, NormalizedRuntimeExportIntent,
    NormalizedSecretIntent, NormalizedSecurityCapabilitiesIntent, NormalizedServiceIntent,
    NormalizedServiceNetworkIntent, NormalizedServiceStateBindingIntent, NormalizedSignalsIntent,
    NormalizedSnapshotIntent, NormalizedStateIntent, NormalizedStateOwnerIntent,
    NormalizedStorageIntent, NormalizedStorageVolumeIntent, NormalizedSurfaceIntent,
    NormalizedTargetIntent, NormalizedTargetsIntent, NormalizedToolDependencyIntent,
    NormalizedTransparencyIntent, NormalizedValueSchemaIntent, OpaqueAuthoredString, OpaqueCommand,
    ProbePortReference, ProgramIdentifier, ProgramManifestIntentV1, RemoteArtifactRef,
    Sha256DigestPin, SourceExistingPath, SourceRelativeFuturePath, SourceRelativePath,
    TcpProbeTarget, WitWorldRef,
};
use crate::execution_contract::GuestPath;
use crate::types::{
    BuildConfig, CapsuleCapabilities, CapsuleExecution, CapsuleExports, CapsuleManifest,
    CapsuleRequirements, CapsuleStorage, CommandSpec, ConfigField, ConfigKind, ContextConfig,
    ContractSpec, DependencySpec, ExternalCapabilitySpec, ExternalCapsuleDependency,
    ExternalInjectionSpec, GeneratedBindingSpec, HostCapabilitySpec, IngressConfig, NamedTarget,
    NetworkConfig, OciTarget, ParamValue, ReadinessProbe, ReadyProbe, RuntimeExportSpec,
    RuntimeType, SecretSpec, ServiceSpec, ShellKind, SnapshotConfig, SourceTarget,
    StateRequirement, TargetsConfig, ToolDependencySpec, WasmTarget,
};

// ─────────────────────────────────────────────────────────────────────────────
// Strict input gate (ADR-014 §2.0.1) — key sets only, values lenient
// ─────────────────────────────────────────────────────────────────────────────

/// Strict structural gate over raw `capsule.toml` text. All 41
/// `CapsuleManifest` top-level fields are explicit; unknown top-level keys
/// fail closed. Values are never interpreted here — the post-normalization
/// model is the sole source of meaning (§2.0.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramManifestV03Input {
    // Non-identity provenance sections stay lenient — the key is known, the
    // value is never gated.
    #[serde(default)]
    pub schema_version: Option<toml::Value>,
    #[serde(default)]
    pub name: Option<toml::Value>,
    #[serde(default)]
    pub version: Option<toml::Value>,
    #[serde(default, rename = "type")]
    pub capsule_type: Option<toml::Value>,
    #[serde(default)]
    pub default_target: Option<toml::Value>,
    #[serde(default)]
    pub metadata: Option<toml::Value>,
    #[serde(default)]
    pub capabilities: Option<CapabilitiesV03Input>,
    #[serde(default)]
    pub requirements: Option<RequirementsV03Input>,
    /// Raw `[execution]` authoring is not part of the accepted v0.3 surface;
    /// presence rejects in [`parse_program_manifest_v03_input`], mirroring the
    /// existing normalizer.
    #[serde(default)]
    pub execution: Option<toml::Value>,
    #[serde(default)]
    pub storage: Option<StorageV03Input>,
    #[serde(default)]
    pub state: BTreeMap<String, StateV03Input>,
    #[serde(default)]
    pub state_owner_scope: Option<toml::Value>,
    #[serde(default)]
    pub service_binding_scope: Option<toml::Value>,
    #[serde(default)]
    pub routing: Option<toml::Value>,
    #[serde(default)]
    pub network: Option<NetworkV03Input>,
    #[serde(default)]
    pub model: Option<ModelSectionV03Input>,
    #[serde(default)]
    pub transparency: Option<TransparencyV03Input>,
    #[serde(default)]
    pub pool: Option<toml::Value>,
    #[serde(default)]
    pub build: Option<BuildV03Input>,
    #[serde(default)]
    pub pack: Option<PackV03Input>,
    #[serde(default)]
    pub isolation: Option<IsolationV03Input>,
    #[serde(default)]
    pub polymorphism: Option<PolymorphismV03Input>,
    #[serde(default)]
    pub targets: Option<ProgramTargetsV03Input>,
    #[serde(default)]
    pub platforms: BTreeMap<String, PlatformArtifactV03Input>,
    #[serde(default)]
    pub exports: Option<ExportsV03Input>,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceV03Input>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DependencyV03Input>,
    #[serde(default)]
    pub tool_dependencies: BTreeMap<String, ToolDependencyV03Input>,
    #[serde(default)]
    pub required_env: Option<toml::Value>,
    #[serde(default)]
    pub contracts: BTreeMap<String, ContractV03Input>,
    /// Unsupported (ADR-014 §2.1): presence fails Program Identity issuance
    /// in the adapter with `UnsupportedField("workspace")`.
    #[serde(default)]
    pub workspace: Option<toml::Value>,
    #[serde(default)]
    pub distribution: Option<toml::Value>,
    #[serde(default)]
    pub foundation_requirements: Option<FoundationRequirementsV03Input>,
    #[serde(default)]
    pub host_capabilities: Vec<HostCapabilityV03Input>,
    #[serde(default)]
    pub ingress: Option<IngressV03Input>,
    #[serde(default)]
    pub snapshot: Option<SnapshotV03Input>,
    #[serde(default)]
    pub secrets: BTreeMap<String, SecretV03Input>,
    #[serde(default)]
    pub bindings: BTreeMap<String, BindingV03Input>,
    #[serde(default)]
    pub external: BTreeMap<String, ExternalV03Input>,
    #[serde(default)]
    pub context: Option<ContextV03Input>,
    #[serde(default)]
    pub generated_bindings: BTreeMap<String, GeneratedBindingV03Input>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesV03Input {
    #[serde(default)]
    pub chat: Option<toml::Value>,
    #[serde(default)]
    pub function_calling: Option<toml::Value>,
    #[serde(default)]
    pub vision: Option<toml::Value>,
    #[serde(default)]
    pub context_length: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementsV03Input {
    #[serde(default)]
    pub platform: Option<toml::Value>,
    #[serde(default)]
    pub vram_min: Option<toml::Value>,
    #[serde(default)]
    pub vram_recommended: Option<toml::Value>,
    #[serde(default)]
    pub disk: Option<toml::Value>,
    #[serde(default)]
    pub dependencies: Option<toml::Value>,
    #[serde(default)]
    pub capabilities: Option<SecurityCapabilitiesV03Input>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityCapabilitiesV03Input {
    #[serde(default)]
    pub network: Option<toml::Value>,
    #[serde(default)]
    pub fs_writes: Option<toml::Value>,
    #[serde(default)]
    pub side_effects: Option<toml::Value>,
    #[serde(default)]
    pub secrets_required: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageV03Input {
    #[serde(default)]
    pub volumes: Vec<StorageVolumeV03Input>,
    #[serde(default)]
    pub use_thin_provisioning: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageVolumeV03Input {
    #[serde(default)]
    pub name: Option<toml::Value>,
    #[serde(default)]
    pub mount_path: Option<toml::Value>,
    #[serde(default)]
    pub read_only: Option<toml::Value>,
    #[serde(default)]
    pub size_bytes: Option<toml::Value>,
    #[serde(default)]
    pub use_thin: Option<toml::Value>,
    #[serde(default)]
    pub encrypted: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateV03Input {
    #[serde(default)]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub durability: Option<toml::Value>,
    #[serde(default)]
    pub purpose: Option<toml::Value>,
    #[serde(default)]
    pub producer: Option<toml::Value>,
    #[serde(default)]
    pub attach: Option<toml::Value>,
    #[serde(default)]
    pub schema_id: Option<toml::Value>,
    #[serde(default)]
    pub sharing: Option<toml::Value>,
    #[serde(default)]
    pub size_mb: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkV03Input {
    #[serde(default)]
    pub egress_allow: Option<toml::Value>,
    #[serde(default)]
    pub egress_id_allow: Vec<EgressIdRuleV03Input>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressIdRuleV03Input {
    #[serde(default, rename = "type")]
    pub rule_type: Option<toml::Value>,
    #[serde(default)]
    pub value: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSectionV03Input {
    #[serde(default)]
    pub source: Option<toml::Value>,
    #[serde(default)]
    pub quantization: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransparencyV03Input {
    #[serde(default)]
    pub level: Option<toml::Value>,
    #[serde(default)]
    pub allowed_binaries: Option<toml::Value>,
}

/// `[build]` accepts the string shorthand (`build = "npm run build"`) exactly
/// as the real deserializer does; the table form is gated strictly.
#[derive(Debug, Clone)]
pub enum BuildV03Input {
    InlineCommand,
    Table(Box<BuildTableV03Input>),
}

impl<'de> Deserialize<'de> for BuildV03Input {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::String(_) => Ok(Self::InlineCommand),
            table @ toml::Value::Table(_) => {
                Ok(Self::Table(table.try_into().map_err(|error| {
                    de::Error::custom(format!("[build]: {error}"))
                })?))
            }
            _ => Err(de::Error::custom(
                "[build] must be a command string or a table",
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTableV03Input {
    #[serde(default)]
    pub exclude_libs: Option<toml::Value>,
    #[serde(default)]
    pub gpu: Option<toml::Value>,
    #[serde(default)]
    pub lifecycle: Option<BuildLifecycleV03Input>,
    #[serde(default)]
    pub inputs: Option<BuildInputsV03Input>,
    #[serde(default)]
    pub outputs: Option<BuildOutputsV03Input>,
    #[serde(default)]
    pub policy: Option<BuildPolicyV03Input>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLifecycleV03Input {
    #[serde(default)]
    pub prepare: Option<toml::Value>,
    #[serde(default)]
    pub build: Option<toml::Value>,
    #[serde(default)]
    pub package: Option<toml::Value>,
    #[serde(default)]
    pub verify: Option<toml::Value>,
    #[serde(default)]
    pub publish: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildInputsV03Input {
    #[serde(default)]
    pub lockfiles: Option<toml::Value>,
    #[serde(default)]
    pub toolchain: Option<toml::Value>,
    #[serde(default)]
    pub artifacts: Option<toml::Value>,
    #[serde(default)]
    pub allow_network: Option<toml::Value>,
    #[serde(default)]
    pub reproducibility: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildOutputsV03Input {
    #[serde(default)]
    pub capsule: Option<toml::Value>,
    #[serde(default)]
    pub sha256: Option<toml::Value>,
    #[serde(default)]
    pub blake3: Option<toml::Value>,
    #[serde(default)]
    pub attestation: Option<toml::Value>,
    #[serde(default)]
    pub signature: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPolicyV03Input {
    #[serde(default)]
    pub require_attestation: Option<toml::Value>,
    #[serde(default)]
    pub require_did_signature: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackV03Input {
    #[serde(default)]
    pub include: Option<toml::Value>,
    #[serde(default)]
    pub exclude: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationV03Input {
    #[serde(default)]
    pub allow_env: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolymorphismV03Input {
    #[serde(default)]
    pub implements: Option<toml::Value>,
}

/// `[targets]` gate. `TargetsConfig`'s `#[serde(flatten)]` named-target
/// pattern defeats `deny_unknown_fields`, so this custom deserializer routes
/// reserved keys to known fields and every other key through the strict
/// named-target struct (ADR-014 §2.0.1).
#[derive(Debug, Clone, Default)]
pub struct ProgramTargetsV03Input {
    pub preference: Option<toml::Value>,
    pub source_digest: Option<toml::Value>,
    pub port: Option<toml::Value>,
    pub startup_timeout: Option<toml::Value>,
    pub env: Option<toml::Value>,
    pub health_check: Option<toml::Value>,
    pub wasm: Option<WasmTargetV03Input>,
    pub source: Option<SourceTargetV03Input>,
    pub oci: Option<OciTargetV03Input>,
    pub named: BTreeMap<String, NamedTargetV03Input>,
}

impl<'de> Deserialize<'de> for ProgramTargetsV03Input {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TargetsVisitor;

        impl<'de> Visitor<'de> for TargetsVisitor {
            type Value = ProgramTargetsV03Input;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a [targets] table")
            }

            fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut out = ProgramTargetsV03Input::default();
                // TOML itself rejects duplicate keys; this guard keeps the
                // gate closed for any non-TOML deserializer front-end.
                let mut seen = BTreeSet::new();
                while let Some(key) = access.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!(
                            "duplicate key '{key}' in [targets]"
                        )));
                    }
                    match key.as_str() {
                        "preference" => out.preference = Some(access.next_value()?),
                        "source_digest" => out.source_digest = Some(access.next_value()?),
                        "port" => out.port = Some(access.next_value()?),
                        "startup_timeout" => out.startup_timeout = Some(access.next_value()?),
                        "env" => out.env = Some(access.next_value()?),
                        "health_check" => out.health_check = Some(access.next_value()?),
                        "wasm" => {
                            let value: toml::Value = access.next_value()?;
                            out.wasm = Some(value.try_into().map_err(|error| {
                                de::Error::custom(format!("[targets.wasm]: {error}"))
                            })?);
                        }
                        "source" => {
                            let value: toml::Value = access.next_value()?;
                            out.source = Some(value.try_into().map_err(|error| {
                                de::Error::custom(format!("[targets.source]: {error}"))
                            })?);
                        }
                        "oci" => {
                            let value: toml::Value = access.next_value()?;
                            out.oci = Some(value.try_into().map_err(|error| {
                                de::Error::custom(format!("[targets.oci]: {error}"))
                            })?);
                        }
                        label => {
                            let value: toml::Value = access.next_value()?;
                            let target = value.try_into().map_err(|error| {
                                de::Error::custom(format!("[targets.{label}]: {error}"))
                            })?;
                            out.named.insert(label.to_string(), target);
                        }
                    }
                }
                Ok(out)
            }
        }

        deserializer.deserialize_map(TargetsVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WasmTargetV03Input {
    #[serde(default)]
    pub digest: Option<toml::Value>,
    #[serde(default)]
    pub world: Option<toml::Value>,
    #[serde(default)]
    pub config: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTargetV03Input {
    #[serde(default)]
    pub language: Option<toml::Value>,
    #[serde(default)]
    pub version: Option<toml::Value>,
    #[serde(default)]
    pub entrypoint: Option<toml::Value>,
    #[serde(default)]
    pub dependencies: Option<toml::Value>,
    #[serde(default)]
    pub args: Option<toml::Value>,
    #[serde(default)]
    pub dev_mode: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciTargetV03Input {
    #[serde(default)]
    pub image: Option<toml::Value>,
    #[serde(default)]
    pub digest: Option<toml::Value>,
    #[serde(default)]
    pub cmd: Option<toml::Value>,
    #[serde(default)]
    pub env: Option<toml::Value>,
    #[serde(default)]
    pub user: Option<toml::Value>,
}

/// Strict gate over one `[targets.<label>]` named target. Mirrors every
/// `NamedTarget` field and serde alias.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedTargetV03Input {
    #[serde(default)]
    pub runtime: Option<toml::Value>,
    #[serde(default)]
    pub surface: Option<SurfaceV03Input>,
    #[serde(default)]
    pub driver: Option<toml::Value>,
    #[serde(default)]
    pub language: Option<toml::Value>,
    #[serde(default)]
    pub runtime_version: Option<toml::Value>,
    #[serde(default)]
    pub runtime_tools: Option<toml::Value>,
    #[serde(default)]
    pub tool_artifacts: Option<toml::Value>,
    #[serde(default)]
    pub entrypoint: Option<toml::Value>,
    #[serde(default)]
    pub image: Option<toml::Value>,
    #[serde(default)]
    pub cmd: Option<toml::Value>,
    #[serde(default)]
    pub env: Option<toml::Value>,
    #[serde(default)]
    pub user: Option<toml::Value>,
    #[serde(default)]
    pub engine: Option<toml::Value>,
    #[serde(default)]
    pub engine_version: Option<toml::Value>,
    #[serde(default)]
    pub engine_variant: Option<toml::Value>,
    #[serde(default)]
    pub engine_path: Option<toml::Value>,
    #[serde(default)]
    pub model: Option<toml::Value>,
    #[serde(default)]
    pub model_url: Option<toml::Value>,
    #[serde(default)]
    pub model_sha256: Option<toml::Value>,
    #[serde(default)]
    pub model_filename: Option<toml::Value>,
    #[serde(default)]
    pub model_format: Option<toml::Value>,
    #[serde(default)]
    pub model_repo: Option<toml::Value>,
    #[serde(default)]
    pub model_revision: Option<toml::Value>,
    #[serde(default)]
    pub model_repo_sha256: Option<toml::Value>,
    #[serde(default)]
    pub model_repo_include: Option<toml::Value>,
    #[serde(default)]
    pub model_repo_gated: Option<toml::Value>,
    #[serde(default)]
    pub server_args: Option<toml::Value>,
    #[serde(default)]
    pub required_env: Option<toml::Value>,
    #[serde(default, alias = "depends_on")]
    pub needs: Option<toml::Value>,
    #[serde(default)]
    pub config_schema: Vec<ConfigFieldV03Input>,
    #[serde(default)]
    pub public: Option<toml::Value>,
    #[serde(default)]
    pub port: Option<toml::Value>,
    #[serde(default)]
    pub working_dir: Option<toml::Value>,
    #[serde(default)]
    pub source_layout: Option<toml::Value>,
    #[serde(default)]
    pub package_type: Option<toml::Value>,
    #[serde(default, alias = "build")]
    pub build_command: Option<toml::Value>,
    #[serde(default, alias = "install")]
    pub install_command: Option<CommandSpecV03Input>,
    #[serde(default, alias = "prestart")]
    pub prestart_command: Option<CommandSpecV03Input>,
    #[serde(default)]
    pub outputs: Option<toml::Value>,
    #[serde(default)]
    pub build_env: Option<toml::Value>,
    #[serde(default, alias = "run")]
    pub run_command: Option<toml::Value>,
    #[serde(default)]
    pub component: Option<toml::Value>,
    #[serde(default)]
    pub readiness_probe: Option<ReadinessProbeV03Input>,
    #[serde(default)]
    pub package_dependencies: Option<toml::Value>,
    #[serde(default)]
    pub external_dependencies: Vec<ExternalDependencyV03Input>,
    #[serde(default)]
    pub external_injection: BTreeMap<String, ExternalInjectionV03Input>,
    #[serde(default)]
    pub env_allowlist: Option<toml::Value>,
    #[serde(default)]
    pub allow_emulation: Option<toml::Value>,
    #[serde(default)]
    pub run_once: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceV03Input {
    #[serde(default)]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub profiles: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFieldV03Input {
    #[serde(default)]
    pub name: Option<toml::Value>,
    #[serde(default)]
    pub label: Option<toml::Value>,
    #[serde(default)]
    pub description: Option<toml::Value>,
    #[serde(default)]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub choices: Option<toml::Value>,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub placeholder: Option<toml::Value>,
}

/// Strict gate over a `CommandSpec` (untagged in the real type — the strict
/// layer refuses a table mixing the `shell` and `cmd` forms, which the
/// tolerant untagged deserializer would silently accept).
#[derive(Debug, Clone)]
pub enum CommandSpecV03Input {
    Raw,
    Shell,
    Argv,
}

impl<'de> Deserialize<'de> for CommandSpecV03Input {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::String(_) => Ok(Self::Raw),
            toml::Value::Table(table) => {
                if table.contains_key("shell") {
                    reject_unknown_keys(&table, &["shell", "shell_kind"], "shell command")?;
                    Ok(Self::Shell)
                } else if table.contains_key("cmd") {
                    reject_unknown_keys(&table, &["cmd", "args", "cwd", "env"], "argv command")?;
                    Ok(Self::Argv)
                } else {
                    Err(de::Error::custom(
                        "command table must declare either 'shell' or 'cmd'",
                    ))
                }
            }
            _ => Err(de::Error::custom(
                "command must be a string or a shell/argv table",
            )),
        }
    }
}

fn reject_unknown_keys<E>(
    table: &toml::value::Table,
    allowed: &[&str],
    context: &str,
) -> Result<(), E>
where
    E: de::Error,
{
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(E::custom(format!("unknown key '{key}' in {context}")));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessProbeV03Input {
    #[serde(default)]
    pub http_get: Option<toml::Value>,
    #[serde(default)]
    pub tcp_connect: Option<toml::Value>,
    #[serde(default)]
    pub exec: Option<toml::Value>,
    #[serde(default)]
    pub port: Option<toml::Value>,
    #[serde(default)]
    pub initial_delay_seconds: Option<toml::Value>,
    #[serde(default)]
    pub timeout_seconds: Option<toml::Value>,
    #[serde(default)]
    pub interval_seconds: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalDependencyV03Input {
    #[serde(default)]
    pub alias: Option<toml::Value>,
    #[serde(default)]
    pub source: Option<toml::Value>,
    #[serde(default)]
    pub source_type: Option<toml::Value>,
    #[serde(default)]
    pub contract: Option<toml::Value>,
    #[serde(default)]
    pub injection_bindings: Option<toml::Value>,
    #[serde(default)]
    pub parameters: Option<toml::Value>,
    #[serde(default)]
    pub credentials: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalInjectionV03Input {
    #[serde(default, rename = "type")]
    pub injection_type: Option<toml::Value>,
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub default: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformArtifactV03Input {
    #[serde(default)]
    pub artifact: Option<toml::Value>,
    #[serde(default)]
    pub sha256: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportsV03Input {
    #[serde(default)]
    pub cli: BTreeMap<String, CliExportV03Input>,
    #[serde(default)]
    pub binaries: Option<toml::Value>,
    #[serde(default)]
    pub paths: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliExportV03Input {
    #[serde(default)]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub target: Option<toml::Value>,
    #[serde(default)]
    pub args: Option<toml::Value>,
    /// Rule-2 exclusion (display-only) — valid authoring, dropped by the
    /// adapter.
    #[serde(default)]
    pub description: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceV03Input {
    #[serde(default, alias = "command")]
    pub entrypoint: Option<toml::Value>,
    #[serde(default)]
    pub target: Option<toml::Value>,
    #[serde(default)]
    pub depends_on: Option<toml::Value>,
    #[serde(default)]
    pub expose: Option<toml::Value>,
    #[serde(default)]
    pub env: Option<toml::Value>,
    #[serde(default)]
    pub secrets: Option<toml::Value>,
    #[serde(default)]
    pub state_bindings: Vec<ServiceStateBindingV03Input>,
    #[serde(default)]
    pub readiness_probe: Option<ReadinessProbeV03Input>,
    #[serde(default)]
    pub network: Option<ServiceNetworkV03Input>,
    #[serde(default)]
    pub run_once: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceStateBindingV03Input {
    #[serde(default)]
    pub state: Option<toml::Value>,
    #[serde(default)]
    pub target: Option<toml::Value>,
    #[serde(default)]
    pub service_target: Option<toml::Value>,
    #[serde(default)]
    pub owner: Option<StateOwnerV03Input>,
    #[serde(default)]
    pub mode: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateOwnerV03Input {
    #[serde(default)]
    pub uid: Option<toml::Value>,
    #[serde(default)]
    pub gid: Option<toml::Value>,
    #[serde(default)]
    pub recursive: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceNetworkV03Input {
    #[serde(default)]
    pub aliases: Option<toml::Value>,
    #[serde(default)]
    pub publish: Option<toml::Value>,
    #[serde(default)]
    pub allow_from: Option<toml::Value>,
    #[serde(default)]
    pub egress_proxy: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyV03Input {
    #[serde(default)]
    pub capsule: Option<toml::Value>,
    #[serde(default)]
    pub contract: Option<toml::Value>,
    #[serde(default)]
    pub parameters: Option<toml::Value>,
    #[serde(default)]
    pub credentials: Option<toml::Value>,
    #[serde(default)]
    pub state: Option<DependencyStateV03Input>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyStateV03Input {
    #[serde(default)]
    pub name: Option<toml::Value>,
    #[serde(default)]
    pub ownership: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDependencyV03Input {
    #[serde(default, rename = "ref")]
    pub capsule_ref: Option<toml::Value>,
    #[serde(default)]
    pub version: Option<toml::Value>,
    #[serde(default)]
    pub bind_env: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractV03Input {
    #[serde(default)]
    pub target: Option<toml::Value>,
    #[serde(default)]
    pub ready: Option<ReadyProbeV03Input>,
    #[serde(default)]
    pub parameters: BTreeMap<String, ValueSchemaV03Input>,
    #[serde(default)]
    pub credentials: BTreeMap<String, ValueSchemaV03Input>,
    #[serde(default)]
    pub identity_exports: Option<toml::Value>,
    #[serde(default)]
    pub runtime_exports: BTreeMap<String, RuntimeExportV03Input>,
    #[serde(default)]
    pub state: Option<ContractStateV03Input>,
}

/// Strict gate over the dependency-grammar `ReadyProbe` (internally tagged —
/// serde cannot `deny_unknown_fields` per variant, and the tolerant real
/// deserializer silently drops keys from the wrong variant).
#[derive(Debug, Clone)]
pub struct ReadyProbeV03Input;

impl<'de> Deserialize<'de> for ReadyProbeV03Input {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        let toml::Value::Table(table) = value else {
            return Err(de::Error::custom("ready probe must be a table"));
        };
        let Some(kind) = table.get("type").and_then(toml::Value::as_str) else {
            return Err(de::Error::custom("ready probe must declare 'type'"));
        };
        let allowed: &[&str] = match kind {
            "tcp" => &["type", "target", "timeout"],
            "probe" => &["type", "run", "timeout"],
            "postgres" => &["type", "host", "port", "user", "database", "timeout"],
            "http" => &["type", "url", "expect_status", "timeout"],
            "unix_socket" => &["type", "path", "timeout"],
            other => {
                return Err(de::Error::custom(format!(
                    "unknown ready probe type '{other}'"
                )));
            }
        };
        reject_unknown_keys(&table, allowed, &format!("ready probe '{kind}'"))?;
        Ok(Self)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueSchemaV03Input {
    #[serde(default, rename = "type")]
    pub value_type: Option<toml::Value>,
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub default: Option<toml::Value>,
}

/// Strict gate over `RuntimeExportSpec` (untagged shorthand string or a
/// `{ value, secret }` table).
#[derive(Debug, Clone)]
pub struct RuntimeExportV03Input;

impl<'de> Deserialize<'de> for RuntimeExportV03Input {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::String(_) => Ok(Self),
            toml::Value::Table(table) => {
                reject_unknown_keys(&table, &["value", "secret"], "runtime export")?;
                Ok(Self)
            }
            _ => Err(de::Error::custom(
                "runtime export must be a string or a { value, secret } table",
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractStateV03Input {
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub version: Option<toml::Value>,
    #[serde(default)]
    pub mount: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationRequirementsV03Input {
    #[serde(default)]
    pub profile: Option<toml::Value>,
    #[serde(default)]
    pub runtimes: Option<toml::Value>,
    #[serde(default)]
    pub engines: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCapabilityV03Input {
    #[serde(default)]
    pub name: Option<toml::Value>,
    #[serde(default)]
    pub reason: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressV03Input {
    #[serde(default)]
    pub mode: Option<toml::Value>,
    #[serde(default)]
    pub routes: BTreeMap<String, IngressRouteV03Input>,
    #[serde(default)]
    pub env_inject: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngressRouteV03Input {
    #[serde(default)]
    pub target: Option<toml::Value>,
    #[serde(default)]
    pub port: Option<toml::Value>,
    #[serde(default)]
    pub listed: Option<toml::Value>,
    #[serde(default)]
    pub alias: Option<toml::Value>,
    #[serde(default)]
    pub strip_prefix: Option<toml::Value>,
    #[serde(default)]
    pub upstream_path_prefix: Option<toml::Value>,
    #[serde(default)]
    pub root: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotV03Input {
    #[serde(default)]
    pub mode: Option<toml::Value>,
    #[serde(default)]
    pub boot_until: Option<toml::Value>,
    #[serde(default)]
    pub sanitize_after_restore: Option<toml::Value>,
    #[serde(default)]
    pub runner_class: Option<toml::Value>,
    #[serde(default)]
    pub max_restore_seconds: Option<toml::Value>,
    #[serde(default)]
    pub warmup_paths: Option<toml::Value>,
    #[serde(default)]
    pub stable_successes: Option<toml::Value>,
    #[serde(default)]
    pub stable_interval_ms: Option<toml::Value>,
    #[serde(default)]
    pub content_ready_path: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretV03Input {
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub description: Option<toml::Value>,
    #[serde(default)]
    pub env: Option<toml::Value>,
    #[serde(default)]
    pub delivery: Option<toml::Value>,
    #[serde(default)]
    pub class: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingV03Input {
    #[serde(default)]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub scope: Option<toml::Value>,
    #[serde(default)]
    pub mount: Option<toml::Value>,
    #[serde(default)]
    pub mode: Option<toml::Value>,
    #[serde(default)]
    pub provider: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalV03Input {
    #[serde(default, rename = "type")]
    pub kind: Option<toml::Value>,
    #[serde(default)]
    pub required: Option<toml::Value>,
    #[serde(default)]
    pub providers: Option<toml::Value>,
    #[serde(default)]
    pub provider: Option<toml::Value>,
    #[serde(default)]
    pub provision: Option<toml::Value>,
    #[serde(default)]
    pub locality: Option<toml::Value>,
    #[serde(default)]
    pub degraded: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextV03Input {
    #[serde(default)]
    pub store: Option<toml::Value>,
    #[serde(default)]
    pub artifacts: Option<toml::Value>,
    #[serde(default)]
    pub index: Option<toml::Value>,
    #[serde(default)]
    pub mount: Option<toml::Value>,
    #[serde(default)]
    pub provenance: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedBindingV03Input {
    #[serde(default)]
    pub generator: Option<toml::Value>,
    #[serde(default)]
    pub bytes: Option<toml::Value>,
    #[serde(default)]
    pub scope: Option<toml::Value>,
    #[serde(default)]
    pub targets: Option<toml::Value>,
}

/// Run the strict structural gate over raw `capsule.toml` text. Rejection
/// only — a success carries no meaning beyond "the key sets are admissible"
/// (ADR-014 §2.0.1).
pub fn parse_program_manifest_v03_input(
    raw_text: &str,
) -> Result<ProgramManifestV03Input, CapsuleProgramError> {
    let input: ProgramManifestV03Input = toml::from_str(raw_text)
        .map_err(|error| CapsuleProgramError::ManifestInput(error.to_string()))?;
    if input.execution.is_some() {
        return Err(CapsuleProgramError::ManifestInput(
            "legacy [execution] section is not part of the accepted v0.3 authoring surface"
                .to_string(),
        ));
    }
    Ok(input)
}

// ─────────────────────────────────────────────────────────────────────────────
// Adapter — post-normalization model → ProgramManifestIntentV1
// ─────────────────────────────────────────────────────────────────────────────

fn invalid_value(field: &'static str, reason: impl Into<String>) -> CapsuleProgramError {
    CapsuleProgramError::InvalidValue {
        field,
        reason: reason.into(),
    }
}

/// The canonical serde spelling of a string-serialized enum value.
fn serde_name<T: Serialize>(field: &'static str, value: &T) -> Result<String, CapsuleProgramError> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        Ok(other) => Err(invalid_value(
            field,
            format!("expected a string-serialized enum value, got {other}"),
        )),
        Err(error) => Err(CapsuleProgramError::Canonicalization(error.to_string())),
    }
}

fn serde_identifier<T: Serialize>(
    field: &'static str,
    value: &T,
) -> Result<ProgramIdentifier, CapsuleProgramError> {
    identifier(field, &serde_name(field, value)?)
}

/// `Some(serde name)` only when the value differs from its normalizer
/// default — absent ≡ explicit default has exactly one canonical spelling.
fn non_default_identifier<T: Serialize + PartialEq>(
    field: &'static str,
    value: &T,
    default: &T,
) -> Result<Option<ProgramIdentifier>, CapsuleProgramError> {
    if value == default {
        Ok(None)
    } else {
        Ok(Some(serde_identifier(field, value)?))
    }
}

fn identifier(field: &'static str, value: &str) -> Result<ProgramIdentifier, CapsuleProgramError> {
    ProgramIdentifier::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn authored(field: &'static str, value: &str) -> Result<OpaqueAuthoredString, CapsuleProgramError> {
    OpaqueAuthoredString::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn command(field: &'static str, value: &str) -> Result<OpaqueCommand, CapsuleProgramError> {
    OpaqueCommand::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn glob(field: &'static str, value: &str) -> Result<GlobPattern, CapsuleProgramError> {
    GlobPattern::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn remote_ref(field: &'static str, value: &str) -> Result<RemoteArtifactRef, CapsuleProgramError> {
    RemoteArtifactRef::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn http_target(field: &'static str, value: &str) -> Result<HttpRequestTarget, CapsuleProgramError> {
    HttpRequestTarget::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn guest_path(field: &'static str, value: &str) -> Result<GuestPath, CapsuleProgramError> {
    GuestPath::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn source_relative(
    field: &'static str,
    value: &str,
) -> Result<SourceRelativePath, CapsuleProgramError> {
    SourceRelativePath::parse(value).map_err(|error| invalid_value(field, error.to_string()))
}

fn future_path(
    field: &'static str,
    value: &str,
) -> Result<SourceRelativeFuturePath, CapsuleProgramError> {
    Ok(SourceRelativeFuturePath(source_relative(field, value)?))
}

#[derive(Clone, Copy)]
enum ExpectedPathKind {
    File,
    FileOrDirectory,
}

/// `SourceExistingPath` policy (ADR-014 §2.2): lexical validation (the
/// grammar itself guarantees containment — no absolute paths, no `..`
/// segments), then the joined path must exist under the selected root as a
/// regular file or directory of the expected kind. Symlink traversal is
/// excluded a priori by the A1v2 admissibility pass over the full tree.
fn existing_path(
    field: &'static str,
    value: &str,
    selected_root: &Path,
    expected: ExpectedPathKind,
) -> Result<SourceExistingPath, CapsuleProgramError> {
    let relative = source_relative(field, value)?;
    let joined = match &relative {
        SourceRelativePath::Root => selected_root.to_path_buf(),
        SourceRelativePath::Relative(path) => selected_root.join(path),
    };
    let metadata = std::fs::symlink_metadata(&joined).map_err(|_| {
        invalid_value(
            field,
            format!("'{value}' does not exist under the selected root"),
        )
    })?;
    let file_type = metadata.file_type();
    let admissible = match expected {
        ExpectedPathKind::File => file_type.is_file(),
        ExpectedPathKind::FileOrDirectory => file_type.is_file() || file_type.is_dir(),
    };
    if !admissible {
        return Err(invalid_value(
            field,
            format!("'{value}' is not a regular file or directory of the expected kind"),
        ));
    }
    Ok(SourceExistingPath(relative))
}

/// Collapse a section whose canonical serialization is `{}` — an authored
/// empty/default section and an absent one must produce the same IR.
fn omit_if_default<T: Serialize>(section: T) -> Result<Option<T>, CapsuleProgramError> {
    let value = serde_json::to_value(&section)
        .map_err(|error| CapsuleProgramError::Canonicalization(error.to_string()))?;
    Ok(match value {
        serde_json::Value::Object(map) if map.is_empty() => None,
        _ => Some(section),
    })
}

fn sorted_set<T: Ord>(mut values: Vec<T>) -> Vec<T> {
    values.sort();
    values.dedup();
    values
}

fn identifier_set(
    field: &'static str,
    values: &[String],
) -> Result<Vec<ProgramIdentifier>, CapsuleProgramError> {
    Ok(sorted_set(
        values
            .iter()
            .map(|value| identifier(field, value))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn glob_set(
    field: &'static str,
    values: &[String],
) -> Result<Vec<GlobPattern>, CapsuleProgramError> {
    Ok(sorted_set(
        values
            .iter()
            .map(|value| glob(field, value))
            .collect::<Result<Vec<_>, _>>()?,
    ))
}

fn env_map(
    field: &'static str,
    values: &HashMap<String, String>,
) -> Result<BTreeMap<ProgramIdentifier, OpaqueAuthoredString>, CapsuleProgramError> {
    values
        .iter()
        .map(|(name, value)| Ok((identifier(field, name)?, authored(field, value)?)))
        .collect()
}

fn param_value(
    field: &'static str,
    value: &ParamValue,
) -> Result<NormalizedParamValueIntent, CapsuleProgramError> {
    Ok(match value {
        ParamValue::String(text) => NormalizedParamValueIntent::String(authored(field, text)?),
        ParamValue::Int(number) => NormalizedParamValueIntent::Int(*number),
        ParamValue::Bool(flag) => NormalizedParamValueIntent::Bool(*flag),
    })
}

/// Runtime class driving the `working_dir` 3-way split (ADR-014 §2.2). The
/// pre-slash selector segment decides (`"source/node"` ⇒ source); every
/// non-OCI, non-Wasm runtime (source, web, native-inference, unknown) is
/// treated as source-like — its working directory is a source-relative path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetRuntimeClass {
    SourceLike,
    Wasm,
    Oci,
}

fn target_runtime_class(runtime: &str) -> TargetRuntimeClass {
    let selector = runtime.trim().to_ascii_lowercase();
    let base = selector.split('/').next().unwrap_or(selector.as_str());
    match base {
        "oci" | "docker" | "youki" => TargetRuntimeClass::Oci,
        "wasm" => TargetRuntimeClass::Wasm,
        _ => TargetRuntimeClass::SourceLike,
    }
}

/// `working_dir`/`cwd` resolution by runtime class. A bare `"/"` OCI workdir
/// fails closed (accepted ADR-014 limitation: `GuestPath` rejects the bare
/// root). A Wasm `working_dir` is a Rule-3 rejection handled by the caller;
/// a Wasm lifecycle-hook `cwd` is classified source-relative (the hooks run
/// against the source tree, and Rule 3 names only `working_dir`).
fn working_dir_intent(
    field: &'static str,
    class: TargetRuntimeClass,
    value: &str,
) -> Result<crate::capsule_program_contract::NormalizedWorkingDir, CapsuleProgramError> {
    use crate::capsule_program_contract::NormalizedWorkingDir;
    Ok(match class {
        TargetRuntimeClass::Oci => NormalizedWorkingDir::Guest(guest_path(field, value)?),
        TargetRuntimeClass::SourceLike | TargetRuntimeClass::Wasm => {
            NormalizedWorkingDir::SourceRelative(source_relative(field, value)?)
        }
    })
}

/// Build the normalized manifest-intent IR from the post-normalization model
/// (ADR-014 §2.0.1: the strict gate over `raw_text` runs FIRST and only
/// rejects; every value consumed afterwards is the normalizer's canonical
/// output, never a re-interpretation of raw TOML).
pub fn program_intent_from_v03(
    model: &CapsuleManifest,
    raw_text: &str,
    selected_root: &Path,
) -> Result<ProgramManifestIntentV1, CapsuleProgramError> {
    let input = parse_program_manifest_v03_input(raw_text)?;
    if input.workspace.is_some() || model.workspace.is_some() {
        return Err(CapsuleProgramError::UnsupportedField("workspace"));
    }

    let default_target = model.default_target.trim();
    let intent = ProgramManifestIntentV1 {
        schema: CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA.to_string(),
        capsule_type: serde_identifier("type", &model.capsule_type)?,
        default_target: if default_target.is_empty() {
            None
        } else {
            Some(identifier("default_target", default_target)?)
        },
        requirements: requirements_intent(&model.requirements)?,
        capabilities: match &model.capabilities {
            Some(capabilities) => capabilities_intent(capabilities)?,
            None => None,
        },
        execution: execution_intent(&model.execution)?,
        storage: storage_intent(&model.storage)?,
        state: model
            .state
            .iter()
            .map(|(name, state)| Ok((identifier("state", name)?, state_intent(state)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        network: match &model.network {
            Some(network) => network_intent(network)?,
            None => None,
        },
        model: match &model.model {
            Some(model_config) => model_section_intent(model_config)?,
            None => None,
        },
        transparency: match &model.transparency {
            Some(transparency) => transparency_intent(transparency)?,
            None => None,
        },
        build: match &model.build {
            Some(build) => build_intent(build, selected_root)?,
            None => None,
        },
        pack: match &model.pack {
            Some(pack) => omit_if_default(NormalizedPackIntent {
                include: pack
                    .include
                    .iter()
                    .map(|pattern| glob("pack.include", pattern))
                    .collect::<Result<_, _>>()?,
                exclude: pack
                    .exclude
                    .iter()
                    .map(|pattern| glob("pack.exclude", pattern))
                    .collect::<Result<_, _>>()?,
            })?,
            None => None,
        },
        isolation: match &model.isolation {
            Some(isolation) => omit_if_default(NormalizedIsolationIntent {
                allow_env: identifier_set("isolation.allow_env", &isolation.allow_env)?,
            })?,
            None => None,
        },
        polymorphism: match &model.polymorphism {
            Some(polymorphism) => omit_if_default(NormalizedPolymorphismIntent {
                implements: identifier_set("polymorphism.implements", &polymorphism.implements)?,
            })?,
            None => None,
        },
        targets: match &model.targets {
            Some(targets) => targets_intent(targets, selected_root)?,
            None => None,
        },
        platforms: model
            .platforms
            .iter()
            .map(|(key, artifact)| {
                Ok((
                    identifier("platforms", key)?,
                    NormalizedPlatformArtifactIntent {
                        artifact: remote_ref("platforms.*.artifact", &artifact.artifact)?,
                        sha256: Sha256DigestPin::parse_flexible(&artifact.sha256).map_err(
                            |error| invalid_value("platforms.*.sha256", error.to_string()),
                        )?,
                    },
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        exports: match &model.exports {
            Some(exports) => exports_intent(exports)?,
            None => None,
        },
        services: match &model.services {
            Some(services) => services
                .iter()
                .map(|(name, service)| {
                    Ok((identifier("services", name)?, service_intent(service)?))
                })
                .collect::<Result<_, CapsuleProgramError>>()?,
            None => BTreeMap::new(),
        },
        dependencies: model
            .dependencies
            .iter()
            .map(|(alias, spec)| Ok((identifier("dependencies", alias)?, dependency_intent(spec)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        tool_dependencies: model
            .tool_dependencies
            .iter()
            .map(|(alias, spec)| {
                Ok((
                    identifier("tool_dependencies", alias)?,
                    tool_dependency_intent(spec)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        required_env: identifier_set("required_env", &model.required_env)?,
        contracts: model
            .contracts
            .iter()
            .map(|(name, spec)| Ok((identifier("contracts", name)?, contract_intent(spec)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        foundation_requirements: match &model.foundation_requirements {
            Some(foundation) => omit_if_default(NormalizedFoundationRequirementsIntent {
                profile: foundation
                    .profile
                    .as_deref()
                    .map(|profile| identifier("foundation_requirements.profile", profile))
                    .transpose()?,
                runtimes: sorted_set(
                    foundation
                        .runtimes
                        .iter()
                        .map(|entry| authored("foundation_requirements.runtimes", entry))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                engines: sorted_set(
                    foundation
                        .engines
                        .iter()
                        .map(|entry| authored("foundation_requirements.engines", entry))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            })?,
            None => None,
        },
        host_capabilities: host_capabilities_intent(&model.host_capabilities)?,
        ingress: match &model.ingress {
            Some(ingress) => Some(ingress_intent(ingress)?),
            None => None,
        },
        snapshot: match &model.snapshot {
            Some(snapshot) => snapshot_intent(snapshot)?,
            None => None,
        },
        secrets: model
            .secrets
            .iter()
            .map(|(name, spec)| Ok((identifier("secrets", name)?, secret_intent(spec)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        bindings: model
            .bindings
            .iter()
            .map(|(name, spec)| Ok((identifier("bindings", name)?, binding_intent(spec)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        external: model
            .external
            .iter()
            .map(|(name, spec)| Ok((identifier("external", name)?, external_intent(spec)?)))
            .collect::<Result<_, CapsuleProgramError>>()?,
        context: match &model.context {
            Some(context) => context_intent(context)?,
            None => None,
        },
        generated_bindings: model
            .generated_bindings
            .iter()
            .map(|(name, spec)| {
                Ok((
                    identifier("generated_bindings", name)?,
                    generated_binding_intent(spec)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
    };
    intent.validate()?;
    Ok(intent)
}

fn requirements_intent(
    requirements: &CapsuleRequirements,
) -> Result<Option<NormalizedRequirementsIntent>, CapsuleProgramError> {
    let capabilities = match &requirements.capabilities {
        Some(capabilities) => omit_if_default(NormalizedSecurityCapabilitiesIntent {
            network: capabilities
                .network
                .as_ref()
                .map(|value| serde_identifier("requirements.capabilities.network", value))
                .transpose()?,
            fs_writes: capabilities
                .fs_writes
                .as_ref()
                .map(|value| serde_identifier("requirements.capabilities.fs_writes", value))
                .transpose()?,
            side_effects: capabilities
                .side_effects
                .as_ref()
                .map(|value| serde_identifier("requirements.capabilities.side_effects", value))
                .transpose()?,
            secrets_required: capabilities.secrets_required,
        })?,
        None => None,
    };
    omit_if_default(NormalizedRequirementsIntent {
        platform: sorted_set(
            requirements
                .platform
                .iter()
                .map(|platform| serde_identifier("requirements.platform", platform))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        vram_min: requirements
            .vram_min
            .as_deref()
            .map(|value| authored("requirements.vram_min", value))
            .transpose()?,
        vram_recommended: requirements
            .vram_recommended
            .as_deref()
            .map(|value| authored("requirements.vram_recommended", value))
            .transpose()?,
        disk: requirements
            .disk
            .as_deref()
            .map(|value| authored("requirements.disk", value))
            .transpose()?,
        dependencies: identifier_set("requirements.dependencies", &requirements.dependencies)?,
        capabilities,
    })
}

fn capabilities_intent(
    capabilities: &CapsuleCapabilities,
) -> Result<Option<NormalizedCapabilitiesIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedCapabilitiesIntent {
        chat: capabilities.chat,
        function_calling: capabilities.function_calling,
        vision: capabilities.vision,
        context_length: capabilities.context_length,
    })
}

/// `execution` is consumed only as the normalizer's canonical derived output;
/// an empty entrypoint means the block is absent (the raw `[execution]`
/// authoring surface is rejected upstream).
fn execution_intent(
    execution: &CapsuleExecution,
) -> Result<Option<NormalizedExecutionIntent>, CapsuleProgramError> {
    if execution.entrypoint.trim().is_empty() {
        return Ok(None);
    }
    let entrypoint = match execution.runtime.normalize() {
        RuntimeType::Oci => NormalizedExecutionEntrypointIntent::OciImage(remote_ref(
            "execution.entrypoint",
            &execution.entrypoint,
        )?),
        _ => NormalizedExecutionEntrypointIntent::SourceRelative(future_path(
            "execution.entrypoint",
            &execution.entrypoint,
        )?),
    };
    let signals = omit_if_default(NormalizedSignalsIntent {
        stop: if execution.signals.stop == "SIGTERM" {
            None
        } else {
            Some(identifier(
                "execution.signals.stop",
                &execution.signals.stop,
            )?)
        },
        kill: if execution.signals.kill == "SIGKILL" {
            None
        } else {
            Some(identifier(
                "execution.signals.kill",
                &execution.signals.kill,
            )?)
        },
    })?;
    Ok(Some(NormalizedExecutionIntent {
        runtime: serde_identifier("execution.runtime", &execution.runtime)?,
        entrypoint,
        port: execution.port,
        health_check: execution
            .health_check
            .as_deref()
            .map(|value| http_target("execution.health_check", value))
            .transpose()?,
        startup_timeout: (execution.startup_timeout != 60).then_some(execution.startup_timeout),
        env: env_map("execution.env", &execution.env)?,
        signals,
    }))
}

fn storage_intent(
    storage: &CapsuleStorage,
) -> Result<Option<NormalizedStorageIntent>, CapsuleProgramError> {
    let mut volumes = storage
        .volumes
        .iter()
        .map(|volume| {
            Ok(NormalizedStorageVolumeIntent {
                name: identifier("storage.volumes.name", &volume.name)?,
                mount_path: guest_path("storage.volumes.mount_path", &volume.mount_path)?,
                read_only: volume.read_only,
                size_bytes: (volume.size_bytes != 0).then_some(volume.size_bytes),
                use_thin: volume.use_thin,
                encrypted: volume.encrypted,
            })
        })
        .collect::<Result<Vec<_>, CapsuleProgramError>>()?;
    volumes.sort_by(|a, b| a.name.cmp(&b.name));
    if volumes.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(invalid_value(
            "storage.volumes",
            "duplicate volume name (ambiguous authoring, fail closed)",
        ));
    }
    omit_if_default(NormalizedStorageIntent {
        volumes,
        use_thin_provisioning: storage.use_thin_provisioning,
    })
}

fn state_intent(state: &StateRequirement) -> Result<NormalizedStateIntent, CapsuleProgramError> {
    Ok(NormalizedStateIntent {
        kind: serde_identifier("state.*.kind", &state.kind)?,
        durability: serde_identifier("state.*.durability", &state.durability)?,
        purpose: authored("state.*.purpose", &state.purpose)?,
        producer: state
            .producer
            .as_deref()
            .map(|producer| identifier("state.*.producer", producer))
            .transpose()?,
        attach: non_default_identifier(
            "state.*.attach",
            &state.attach,
            &crate::types::StateAttach::default(),
        )?,
        schema_id: state
            .schema_id
            .as_deref()
            .map(|schema_id| identifier("state.*.schema_id", schema_id))
            .transpose()?,
        sharing: non_default_identifier(
            "state.*.sharing",
            &state.sharing,
            &crate::types::StateSharing::default(),
        )?,
        size_mb: state.size_mb,
    })
}

fn network_intent(
    network: &NetworkConfig,
) -> Result<Option<NormalizedNetworkIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedNetworkIntent {
        egress_allow: sorted_set(
            network
                .egress_allow
                .iter()
                .map(|entry| remote_ref("network.egress_allow", entry))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        egress_id_allow: sorted_set(
            network
                .egress_id_allow
                .iter()
                .map(|rule| {
                    Ok(NormalizedEgressIdRuleIntent {
                        rule_type: serde_identifier(
                            "network.egress_id_allow.type",
                            &rule.rule_type,
                        )?,
                        value: authored("network.egress_id_allow.value", &rule.value)?,
                    })
                })
                .collect::<Result<Vec<_>, CapsuleProgramError>>()?,
        ),
    })
}

fn model_section_intent(
    model: &crate::types::ModelConfig,
) -> Result<Option<NormalizedModelIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedModelIntent {
        source: model
            .source
            .as_deref()
            .map(|source| remote_ref("model.source", source))
            .transpose()?,
        quantization: model
            .quantization
            .as_ref()
            .map(|quantization| serde_identifier("model.quantization", quantization))
            .transpose()?,
    })
}

fn transparency_intent(
    transparency: &crate::types::TransparencyConfig,
) -> Result<Option<NormalizedTransparencyIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedTransparencyIntent {
        level: non_default_identifier(
            "transparency.level",
            &transparency.level,
            &crate::types::TransparencyLevel::default(),
        )?,
        allowed_binaries: glob_set(
            "transparency.allowed_binaries",
            &transparency.allowed_binaries,
        )?,
    })
}

/// `build.outputs.*` / `build.policy.*` are Rule-2 exclusions — read but
/// never mapped. `build.inputs.lockfiles` are existence-checked; `artifacts`
/// stay lexical-only.
fn build_intent(
    build: &BuildConfig,
    selected_root: &Path,
) -> Result<Option<NormalizedBuildIntent>, CapsuleProgramError> {
    let lifecycle = match &build.lifecycle {
        Some(lifecycle) => {
            let map_command = |field: &'static str, value: &Option<String>| {
                value
                    .as_deref()
                    .map(|text| command(field, text))
                    .transpose()
            };
            omit_if_default(NormalizedBuildLifecycleIntent {
                prepare: map_command("build.lifecycle.prepare", &lifecycle.prepare)?,
                build: map_command("build.lifecycle.build", &lifecycle.build)?,
                package: map_command("build.lifecycle.package", &lifecycle.package)?,
                verify: map_command("build.lifecycle.verify", &lifecycle.verify)?,
                publish: map_command("build.lifecycle.publish", &lifecycle.publish)?,
            })?
        }
        None => None,
    };
    let inputs = match &build.inputs {
        Some(inputs) => omit_if_default(NormalizedBuildInputsIntent {
            lockfiles: sorted_set(
                inputs
                    .lockfiles
                    .iter()
                    .map(|lockfile| {
                        existing_path(
                            "build.inputs.lockfiles",
                            lockfile,
                            selected_root,
                            ExpectedPathKind::File,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            toolchain: inputs
                .toolchain
                .as_deref()
                .map(|toolchain| authored("build.inputs.toolchain", toolchain))
                .transpose()?,
            artifacts: sorted_set(
                inputs
                    .artifacts
                    .iter()
                    .map(|artifact| future_path("build.inputs.artifacts", artifact))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            allow_network: inputs.allow_network,
            reproducibility: inputs
                .reproducibility
                .as_deref()
                .map(|value| authored("build.inputs.reproducibility", value))
                .transpose()?,
        })?,
        None => None,
    };
    omit_if_default(NormalizedBuildIntent {
        exclude_libs: build
            .exclude_libs
            .iter()
            .map(|pattern| glob("build.exclude_libs", pattern))
            .collect::<Result<_, _>>()?,
        gpu: build.gpu,
        lifecycle,
        inputs,
    })
}

fn targets_intent(
    targets: &TargetsConfig,
    selected_root: &Path,
) -> Result<Option<NormalizedTargetsIntent>, CapsuleProgramError> {
    let mut map: BTreeMap<ProgramIdentifier, NormalizedTargetIntent> = BTreeMap::new();
    for (label, named) in &targets.named {
        map.insert(
            identifier("targets", label)?,
            named_target_intent(named, selected_root)?,
        );
    }
    let mut insert_structured =
        |label: &'static str, intent: NormalizedTargetIntent| -> Result<(), CapsuleProgramError> {
            let key = identifier("targets", label)?;
            if map.contains_key(&key) {
                return Err(invalid_value(
                    "targets",
                    format!("structured [targets.{label}] collides with a named target '{label}'"),
                ));
            }
            map.insert(key, intent);
            Ok(())
        };
    if let Some(wasm) = &targets.wasm {
        insert_structured("wasm", wasm_target_intent(wasm)?)?;
    }
    if let Some(source) = &targets.source {
        insert_structured("source", source_target_intent(source, selected_root)?)?;
    }
    if let Some(oci) = &targets.oci {
        insert_structured("oci", oci_target_intent(oci)?)?;
    }
    omit_if_default(NormalizedTargetsIntent {
        preference: targets
            .preference
            .iter()
            .map(|label| identifier("targets.preference", label))
            .collect::<Result<_, _>>()?,
        source_digest: targets
            .source_digest
            .as_deref()
            .map(|digest| {
                Sha256DigestPin::parse_prefixed(digest)
                    .map_err(|error| invalid_value("targets.source_digest", error.to_string()))
            })
            .transpose()?,
        port: targets.port,
        startup_timeout: (targets.startup_timeout != 60).then_some(targets.startup_timeout),
        env: env_map("targets.env", &targets.env)?,
        health_check: targets
            .health_check
            .as_deref()
            .map(|value| http_target("targets.health_check", value))
            .transpose()?,
        targets: map,
    })
}

fn empty_target_intent() -> NormalizedTargetIntent {
    NormalizedTargetIntent {
        runtime: None,
        surface: None,
        driver: None,
        language: None,
        runtime_version: None,
        version: None,
        runtime_tools: BTreeMap::new(),
        tool_artifacts: Vec::new(),
        entrypoint: None,
        component: None,
        image: None,
        digest: None,
        world: None,
        config: BTreeMap::new(),
        cmd: Vec::new(),
        args: Vec::new(),
        env: BTreeMap::new(),
        user: None,
        working_dir: None,
        port: None,
        dependencies: None,
        dev_mode: false,
        engine: None,
        engine_version: None,
        engine_variant: None,
        model: None,
        model_url: None,
        model_sha256: None,
        model_repo: None,
        model_revision: None,
        model_repo_sha256: None,
        model_repo_include: Vec::new(),
        model_repo_gated: false,
        server_args: Vec::new(),
        required_env: Vec::new(),
        config_schema: Vec::new(),
        env_allowlist: Vec::new(),
        public: Vec::new(),
        source_layout: None,
        package_type: None,
        build_command: None,
        install_command: None,
        prestart_command: None,
        run_command: None,
        outputs: Vec::new(),
        build_env: Vec::new(),
        needs: Vec::new(),
        readiness_probe: None,
        package_dependencies: Vec::new(),
        external_dependencies: Vec::new(),
        external_injection: BTreeMap::new(),
        allow_emulation: false,
        run_once: false,
    }
}

fn named_target_intent(
    target: &NamedTarget,
    selected_root: &Path,
) -> Result<NormalizedTargetIntent, CapsuleProgramError> {
    if target.engine_path.is_some() {
        return Err(CapsuleProgramError::UnsupportedField("engine_path"));
    }
    let class = target_runtime_class(&target.runtime);
    let working_dir = match target.working_dir.as_deref() {
        Some(_) if class == TargetRuntimeClass::Wasm => {
            return Err(CapsuleProgramError::UnsupportedField("wasm working_dir"));
        }
        Some(value) => Some(working_dir_intent("targets.*.working_dir", class, value)?),
        None => None,
    };
    let mut external_dependencies = target
        .external_dependencies
        .iter()
        .map(external_dependency_intent)
        .collect::<Result<Vec<_>, _>>()?;
    external_dependencies.sort_by(|a, b| a.alias.cmp(&b.alias));
    if external_dependencies
        .windows(2)
        .any(|pair| pair[0].alias == pair[1].alias)
    {
        return Err(invalid_value(
            "targets.*.external_dependencies",
            "duplicate alias (ambiguous authoring, fail closed)",
        ));
    }
    Ok(NormalizedTargetIntent {
        runtime: if target.runtime.trim().is_empty() {
            None
        } else {
            Some(authored("targets.*.runtime", &target.runtime)?)
        },
        surface: target.surface.as_ref().map(surface_intent).transpose()?,
        driver: target
            .driver
            .as_deref()
            .map(|driver| authored("targets.*.driver", driver))
            .transpose()?,
        language: target
            .language
            .as_deref()
            .map(|language| authored("targets.*.language", language))
            .transpose()?,
        runtime_version: target
            .runtime_version
            .as_deref()
            .map(|version| authored("targets.*.runtime_version", version))
            .transpose()?,
        runtime_tools: target
            .runtime_tools
            .iter()
            .map(|(name, version)| {
                Ok((
                    identifier("targets.*.runtime_tools", name)?,
                    authored("targets.*.runtime_tools", version)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        tool_artifacts: identifier_set("targets.*.tool_artifacts", &target.tool_artifacts)?,
        entrypoint: if target.entrypoint.is_empty() {
            None
        } else {
            Some(future_path("targets.*.entrypoint", &target.entrypoint)?)
        },
        component: target
            .component
            .as_deref()
            .map(|component| future_path("targets.*.component", component))
            .transpose()?,
        image: target
            .image
            .as_deref()
            .map(|image| remote_ref("targets.*.image", image))
            .transpose()?,
        cmd: target
            .cmd
            .iter()
            .map(|entry| command("targets.*.cmd", entry))
            .collect::<Result<_, _>>()?,
        env: env_map("targets.*.env", &target.env)?,
        user: target
            .user
            .as_deref()
            .map(|user| {
                ContainerUserSpec::parse(user)
                    .map_err(|error| invalid_value("targets.*.user", error.to_string()))
            })
            .transpose()?,
        working_dir,
        port: target.port,
        engine: target
            .engine
            .as_deref()
            .map(|engine| remote_ref("targets.*.engine", engine))
            .transpose()?,
        engine_version: target
            .engine_version
            .as_deref()
            .map(|version| authored("targets.*.engine_version", version))
            .transpose()?,
        engine_variant: target
            .engine_variant
            .as_deref()
            .map(|variant| authored("targets.*.engine_variant", variant))
            .transpose()?,
        model: target
            .model
            .as_deref()
            .map(|model| {
                existing_path(
                    "targets.*.model",
                    model,
                    selected_root,
                    ExpectedPathKind::FileOrDirectory,
                )
            })
            .transpose()?,
        model_url: target
            .model_url
            .as_deref()
            .map(|url| remote_ref("targets.*.model_url", url))
            .transpose()?,
        model_sha256: target
            .model_sha256
            .as_deref()
            .map(|pin| {
                Sha256DigestPin::parse_flexible(pin)
                    .map_err(|error| invalid_value("targets.*.model_sha256", error.to_string()))
            })
            .transpose()?,
        model_repo: target
            .model_repo
            .as_deref()
            .map(|repo| remote_ref("targets.*.model_repo", repo))
            .transpose()?,
        model_revision: target
            .model_revision
            .as_deref()
            .map(|revision| {
                GitCommitRevision::parse(revision)
                    .map_err(|error| invalid_value("targets.*.model_revision", error.to_string()))
            })
            .transpose()?,
        model_repo_sha256: target
            .model_repo_sha256
            .as_deref()
            .map(|pin| {
                Sha256DigestPin::parse_flexible(pin).map_err(|error| {
                    invalid_value("targets.*.model_repo_sha256", error.to_string())
                })
            })
            .transpose()?,
        model_repo_include: glob_set("targets.*.model_repo_include", &target.model_repo_include)?,
        model_repo_gated: target.model_repo_gated,
        server_args: target
            .server_args
            .iter()
            .map(|entry| command("targets.*.server_args", entry))
            .collect::<Result<_, _>>()?,
        required_env: identifier_set("targets.*.required_env", &target.required_env)?,
        config_schema: target
            .config_schema
            .iter()
            .map(config_field_intent)
            .collect::<Result<_, _>>()?,
        env_allowlist: identifier_set("targets.*.env_allowlist", &target.env_allowlist)?,
        public: glob_set("targets.*.public", &target.public)?,
        source_layout: target
            .source_layout
            .as_deref()
            .map(|layout| authored("targets.*.source_layout", layout))
            .transpose()?,
        package_type: target
            .package_type
            .as_deref()
            .map(|package_type| authored("targets.*.package_type", package_type))
            .transpose()?,
        build_command: target
            .build_command
            .as_deref()
            .map(|build| command("targets.*.build_command", build))
            .transpose()?,
        install_command: target
            .install_command
            .as_ref()
            .map(|spec| command_intent("targets.*.install_command", class, spec))
            .transpose()?,
        prestart_command: target
            .prestart_command
            .as_ref()
            .map(|spec| command_intent("targets.*.prestart_command", class, spec))
            .transpose()?,
        run_command: target
            .run_command
            .as_deref()
            .map(|run| command("targets.*.run_command", run))
            .transpose()?,
        outputs: sorted_set(
            target
                .outputs
                .iter()
                .map(|output| future_path("targets.*.outputs", output))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        build_env: identifier_set("targets.*.build_env", &target.build_env)?,
        needs: identifier_set("targets.*.needs", &target.needs)?,
        readiness_probe: target
            .readiness_probe
            .as_ref()
            .map(readiness_probe_intent)
            .transpose()?,
        package_dependencies: identifier_set(
            "targets.*.package_dependencies",
            &target.package_dependencies,
        )?,
        external_dependencies,
        external_injection: target
            .external_injection
            .iter()
            .map(|(name, spec)| {
                Ok((
                    identifier("targets.*.external_injection", name)?,
                    external_injection_intent(spec)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        allow_emulation: target.allow_emulation,
        run_once: target.run_once,
        ..empty_target_intent()
    })
}

/// Structured targets canonicalize INTO the named-target IR shape under the
/// reserved labels, with the runtime selector injected so a structured and a
/// named spelling of the same intent produce the same IR.
fn wasm_target_intent(target: &WasmTarget) -> Result<NormalizedTargetIntent, CapsuleProgramError> {
    Ok(NormalizedTargetIntent {
        runtime: Some(authored("targets.wasm", "wasm")?),
        digest: Some(
            CasContentDigest::parse(&target.digest)
                .map_err(|error| invalid_value("targets.wasm.digest", error.to_string()))?,
        ),
        world: Some(
            WitWorldRef::parse(&target.world)
                .map_err(|error| invalid_value("targets.wasm.world", error.to_string()))?,
        ),
        config: target
            .config
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("targets.wasm.config", name)?,
                    authored("targets.wasm.config", value)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        ..empty_target_intent()
    })
}

fn source_target_intent(
    target: &SourceTarget,
    selected_root: &Path,
) -> Result<NormalizedTargetIntent, CapsuleProgramError> {
    Ok(NormalizedTargetIntent {
        runtime: Some(authored("targets.source", "source")?),
        language: Some(authored("targets.source.language", &target.language)?),
        version: target
            .version
            .as_deref()
            .map(|version| authored("targets.source.version", version))
            .transpose()?,
        entrypoint: Some(future_path(
            "targets.source.entrypoint",
            &target.entrypoint,
        )?),
        dependencies: target
            .dependencies
            .as_deref()
            .map(|dependencies| {
                existing_path(
                    "targets.source.dependencies",
                    dependencies,
                    selected_root,
                    ExpectedPathKind::File,
                )
            })
            .transpose()?,
        args: target
            .args
            .iter()
            .map(|arg| command("targets.source.args", arg))
            .collect::<Result<_, _>>()?,
        dev_mode: target.dev_mode,
        ..empty_target_intent()
    })
}

fn oci_target_intent(target: &OciTarget) -> Result<NormalizedTargetIntent, CapsuleProgramError> {
    Ok(NormalizedTargetIntent {
        runtime: Some(authored("targets.oci", "oci")?),
        image: Some(remote_ref("targets.oci.image", &target.image)?),
        digest: target
            .digest
            .as_deref()
            .map(|digest| {
                CasContentDigest::parse(digest)
                    .map_err(|error| invalid_value("targets.oci.digest", error.to_string()))
            })
            .transpose()?,
        cmd: target
            .cmd
            .iter()
            .map(|entry| command("targets.oci.cmd", entry))
            .collect::<Result<_, _>>()?,
        env: env_map("targets.oci.env", &target.env)?,
        user: target
            .user
            .as_deref()
            .map(|user| {
                ContainerUserSpec::parse(user)
                    .map_err(|error| invalid_value("targets.oci.user", error.to_string()))
            })
            .transpose()?,
        ..empty_target_intent()
    })
}

fn surface_intent(
    surface: &crate::types::SessionSurfaceRequirement,
) -> Result<NormalizedSurfaceIntent, CapsuleProgramError> {
    let kind = serde_name("targets.*.surface.kind", &surface.kind)?;
    // `Unknown` is serde(other) forward-compat: hashing it would conflate
    // every unrecognized kind into one value — fail closed instead.
    if kind == "unknown" {
        return Err(invalid_value(
            "targets.*.surface.kind",
            "unrecognized surface kind (fail closed)",
        ));
    }
    Ok(NormalizedSurfaceIntent {
        kind: identifier("targets.*.surface.kind", &kind)?,
        profiles: surface
            .profiles
            .as_ref()
            .map(|profiles| {
                profiles
                    .iter()
                    .map(|profile| identifier("targets.*.surface.profiles", profile))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
    })
}

fn config_field_intent(
    field: &ConfigField,
) -> Result<NormalizedConfigFieldIntent, CapsuleProgramError> {
    let (kind, choices) = match &field.kind {
        ConfigKind::Secret => ("secret", Vec::new()),
        ConfigKind::String => ("string", Vec::new()),
        ConfigKind::Number => ("number", Vec::new()),
        ConfigKind::Enum { choices } => ("enum", choices.clone()),
        // serde(other) forward-compat catch-all — an unrecognized kind cannot
        // be hashed faithfully, fail closed.
        ConfigKind::Unknown => {
            return Err(invalid_value(
                "targets.*.config_schema.kind",
                "unrecognized config field kind (fail closed)",
            ));
        }
    };
    Ok(NormalizedConfigFieldIntent {
        name: identifier("targets.*.config_schema.name", &field.name)?,
        label: field
            .label
            .as_deref()
            .map(|label| authored("targets.*.config_schema.label", label))
            .transpose()?,
        description: field
            .description
            .as_deref()
            .map(|description| authored("targets.*.config_schema.description", description))
            .transpose()?,
        kind: identifier("targets.*.config_schema.kind", kind)?,
        choices: choices
            .iter()
            .map(|choice| authored("targets.*.config_schema.choices", choice))
            .collect::<Result<_, _>>()?,
        default: field
            .default
            .as_deref()
            .map(|default| authored("targets.*.config_schema.default", default))
            .transpose()?,
        placeholder: field
            .placeholder
            .as_deref()
            .map(|placeholder| authored("targets.*.config_schema.placeholder", placeholder))
            .transpose()?,
    })
}

fn command_intent(
    field: &'static str,
    class: TargetRuntimeClass,
    spec: &CommandSpec,
) -> Result<NormalizedCommandIntent, CapsuleProgramError> {
    Ok(match spec {
        CommandSpec::Shell { shell, shell_kind } => NormalizedCommandIntent::Shell {
            shell: command(field, shell)?,
            shell_kind: if *shell_kind == ShellKind::PosixSh {
                None
            } else {
                Some(serde_identifier(field, shell_kind)?)
            },
        },
        CommandSpec::Argv {
            cmd,
            args,
            cwd,
            env,
        } => NormalizedCommandIntent::Argv {
            cmd: command(field, cmd)?,
            args: args
                .iter()
                .map(|arg| command(field, arg))
                .collect::<Result<_, _>>()?,
            cwd: cwd
                .as_deref()
                .map(|cwd| working_dir_intent(field, class, cwd))
                .transpose()?,
            env: env_map(field, env)?,
        },
        CommandSpec::String(raw) => NormalizedCommandIntent::Raw(command(field, raw)?),
    })
}

fn readiness_probe_intent(
    probe: &ReadinessProbe,
) -> Result<NormalizedReadinessProbeIntent, CapsuleProgramError> {
    Ok(NormalizedReadinessProbeIntent {
        http_get: probe
            .http_get
            .as_deref()
            .map(|target| http_target("readiness_probe.http_get", target))
            .transpose()?,
        tcp_connect: probe
            .tcp_connect
            .as_deref()
            .map(|target| {
                TcpProbeTarget::parse(target).map_err(|error| {
                    invalid_value("readiness_probe.tcp_connect", error.to_string())
                })
            })
            .transpose()?,
        exec: probe
            .exec
            .as_ref()
            .map(|argv| {
                argv.iter()
                    .map(|entry| command("readiness_probe.exec", entry))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?,
        port: probe
            .port
            .as_deref()
            .map(|port| {
                ProbePortReference::parse(port)
                    .map_err(|error| invalid_value("readiness_probe.port", error.to_string()))
            })
            .transpose()?,
        initial_delay_seconds: (probe.initial_delay_seconds != 0)
            .then_some(probe.initial_delay_seconds),
        timeout_seconds: (probe.timeout_seconds != 180).then_some(probe.timeout_seconds),
        interval_seconds: (probe.interval_seconds != 2).then_some(probe.interval_seconds),
    })
}

fn external_dependency_intent(
    dependency: &ExternalCapsuleDependency,
) -> Result<NormalizedExternalDependencyIntent, CapsuleProgramError> {
    Ok(NormalizedExternalDependencyIntent {
        alias: identifier("targets.*.external_dependencies.alias", &dependency.alias)?,
        source: remote_ref("targets.*.external_dependencies.source", &dependency.source)?,
        source_type: identifier(
            "targets.*.external_dependencies.source_type",
            &dependency.source_type,
        )?,
        contract: dependency
            .contract
            .as_deref()
            .map(|contract| identifier("targets.*.external_dependencies.contract", contract))
            .transpose()?,
        injection_bindings: dependency
            .injection_bindings
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("targets.*.external_dependencies.injection_bindings", name)?,
                    authored("targets.*.external_dependencies.injection_bindings", value)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        parameters: dependency
            .parameters
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("targets.*.external_dependencies.parameters", name)?,
                    param_value("targets.*.external_dependencies.parameters", value)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        credentials: dependency
            .credentials
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("targets.*.external_dependencies.credentials", name)?,
                    value.clone(),
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
    })
}

fn external_injection_intent(
    spec: &ExternalInjectionSpec,
) -> Result<NormalizedExternalInjectionIntent, CapsuleProgramError> {
    Ok(NormalizedExternalInjectionIntent {
        injection_type: identifier("targets.*.external_injection.type", &spec.injection_type)?,
        required: spec.required,
        default: spec
            .default
            .as_deref()
            .map(|default| authored("targets.*.external_injection.default", default))
            .transpose()?,
    })
}

fn exports_intent(
    exports: &CapsuleExports,
) -> Result<Option<NormalizedExportsIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedExportsIntent {
        cli: exports
            .cli
            .iter()
            .map(|(name, spec)| {
                // `description` is a Rule-2 exclusion (display-only).
                Ok((
                    identifier("exports.cli", name)?,
                    NormalizedCliExportIntent {
                        kind: identifier("exports.cli.*.kind", &spec.kind)?,
                        target: identifier("exports.cli.*.target", &spec.target)?,
                        args: spec
                            .args
                            .iter()
                            .map(|arg| command("exports.cli.*.args", arg))
                            .collect::<Result<_, _>>()?,
                    },
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        binaries: exports
            .binaries
            .iter()
            .map(|(alias, path)| {
                Ok((
                    identifier("exports.binaries", alias)?,
                    future_path("exports.binaries", path)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        paths: exports
            .paths
            .iter()
            .map(|(alias, path)| {
                Ok((
                    identifier("exports.paths", alias)?,
                    future_path("exports.paths", path)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
    })
}

fn service_intent(service: &ServiceSpec) -> Result<NormalizedServiceIntent, CapsuleProgramError> {
    let mut state_bindings = service
        .state_bindings
        .iter()
        .map(|binding| {
            Ok(NormalizedServiceStateBindingIntent {
                state: identifier("services.*.state_bindings.state", &binding.state)?,
                target: guest_path("services.*.state_bindings.target", &binding.target)?,
                service_target: binding
                    .service_target
                    .as_deref()
                    .map(|target| identifier("services.*.state_bindings.service_target", target))
                    .transpose()?,
                owner: binding
                    .owner
                    .as_ref()
                    .map(|owner| NormalizedStateOwnerIntent {
                        uid: owner.uid,
                        gid: owner.gid,
                        recursive: owner.recursive,
                    }),
                mode: binding
                    .mode
                    .as_deref()
                    .map(|mode| authored("services.*.state_bindings.mode", mode))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, CapsuleProgramError>>()?;
    state_bindings.sort_by(|a, b| (&a.state, &a.target).cmp(&(&b.state, &b.target)));
    if state_bindings
        .windows(2)
        .any(|pair| (&pair[0].state, &pair[0].target) == (&pair[1].state, &pair[1].target))
    {
        return Err(invalid_value(
            "services.*.state_bindings",
            "duplicate (state, target) binding (ambiguous authoring, fail closed)",
        ));
    }
    let network = match &service.network {
        Some(network) => omit_if_default(NormalizedServiceNetworkIntent {
            aliases: identifier_set("services.*.network.aliases", &network.aliases)?,
            publish: network.publish,
            allow_from: identifier_set("services.*.network.allow_from", &network.allow_from)?,
            egress_proxy: network.egress_proxy,
        })?,
        None => None,
    };
    Ok(NormalizedServiceIntent {
        entrypoint: if service.entrypoint.trim().is_empty() {
            None
        } else {
            Some(command("services.*.entrypoint", &service.entrypoint)?)
        },
        target: service
            .target
            .as_deref()
            .map(|target| identifier("services.*.target", target))
            .transpose()?,
        depends_on: identifier_set(
            "services.*.depends_on",
            service.depends_on.as_deref().unwrap_or(&[]),
        )?,
        expose: identifier_set(
            "services.*.expose",
            service.expose.as_deref().unwrap_or(&[]),
        )?,
        env: match &service.env {
            Some(env) => env_map("services.*.env", env)?,
            None => BTreeMap::new(),
        },
        secrets: identifier_set(
            "services.*.secrets",
            service.secrets.as_deref().unwrap_or(&[]),
        )?,
        state_bindings,
        readiness_probe: service
            .readiness_probe
            .as_ref()
            .map(readiness_probe_intent)
            .transpose()?,
        network,
        run_once: service.run_once,
    })
}

fn dependency_intent(
    spec: &DependencySpec,
) -> Result<NormalizedDependencyIntent, CapsuleProgramError> {
    Ok(NormalizedDependencyIntent {
        capsule: remote_ref("dependencies.*.capsule", &spec.capsule.0)?,
        contract: identifier("dependencies.*.contract", &spec.contract.to_string())?,
        parameters: spec
            .parameters
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("dependencies.*.parameters", name)?,
                    param_value("dependencies.*.parameters", value)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        credentials: spec
            .credentials
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("dependencies.*.credentials", name)?,
                    value.clone(),
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        state: spec
            .state
            .as_ref()
            .map(|state| {
                Ok::<_, CapsuleProgramError>(NormalizedDependencyStateIntent {
                    name: identifier("dependencies.*.state.name", &state.name)?,
                    // `parent` is the only (and default) ownership — omitted.
                    ownership: None,
                })
            })
            .transpose()?,
    })
}

fn tool_dependency_intent(
    spec: &ToolDependencySpec,
) -> Result<NormalizedToolDependencyIntent, CapsuleProgramError> {
    Ok(NormalizedToolDependencyIntent {
        capsule_ref: remote_ref("tool_dependencies.*.ref", &spec.capsule_ref.0)?,
        version: spec
            .version
            .as_deref()
            .map(|version| authored("tool_dependencies.*.version", version))
            .transpose()?,
        bind_env: spec
            .bind_env
            .iter()
            .map(|(export, env_name)| {
                Ok((
                    identifier("tool_dependencies.*.bind_env", export)?,
                    identifier("tool_dependencies.*.bind_env", env_name)?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
    })
}

fn ready_probe_intent(
    probe: &ReadyProbe,
) -> Result<NormalizedReadyProbeIntent, CapsuleProgramError> {
    let timeout = |timeout: &Option<String>| {
        timeout
            .as_deref()
            .map(|value| authored("contracts.*.ready.timeout", value))
            .transpose()
    };
    Ok(match probe {
        ReadyProbe::Tcp {
            target,
            timeout: probe_timeout,
        } => NormalizedReadyProbeIntent::Tcp {
            target: target.clone(),
            timeout: timeout(probe_timeout)?,
        },
        ReadyProbe::Probe {
            run,
            timeout: probe_timeout,
        } => NormalizedReadyProbeIntent::Probe {
            run: run.clone(),
            timeout: timeout(probe_timeout)?,
        },
        ReadyProbe::Postgres {
            host,
            port,
            user,
            database,
            timeout: probe_timeout,
        } => NormalizedReadyProbeIntent::Postgres {
            host: host.clone(),
            port: port.clone(),
            user: user.clone(),
            database: database.clone(),
            timeout: timeout(probe_timeout)?,
        },
        ReadyProbe::Http {
            url,
            expect_status,
            timeout: probe_timeout,
        } => NormalizedReadyProbeIntent::Http {
            url: url.clone(),
            expect_status: *expect_status,
            timeout: timeout(probe_timeout)?,
        },
        ReadyProbe::UnixSocket {
            path,
            timeout: probe_timeout,
        } => NormalizedReadyProbeIntent::UnixSocket {
            path: path.clone(),
            timeout: timeout(probe_timeout)?,
        },
    })
}

fn contract_intent(spec: &ContractSpec) -> Result<NormalizedContractIntent, CapsuleProgramError> {
    let value_schema = |field: &'static str,
                        value_type: &crate::types::ValueType,
                        required: bool,
                        default: &Option<ParamValue>|
     -> Result<NormalizedValueSchemaIntent, CapsuleProgramError> {
        Ok(NormalizedValueSchemaIntent {
            value_type: serde_identifier(field, value_type)?,
            required,
            default: default
                .as_ref()
                .map(|value| param_value(field, value))
                .transpose()?,
        })
    };
    Ok(NormalizedContractIntent {
        target: identifier("contracts.*.target", &spec.target)?,
        ready: ready_probe_intent(&spec.ready)?,
        parameters: spec
            .parameters
            .iter()
            .map(|(name, schema)| {
                Ok((
                    identifier("contracts.*.parameters", name)?,
                    value_schema(
                        "contracts.*.parameters",
                        &schema.value_type,
                        schema.required,
                        &schema.default,
                    )?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        credentials: spec
            .credentials
            .iter()
            .map(|(name, schema)| {
                Ok((
                    identifier("contracts.*.credentials", name)?,
                    value_schema(
                        "contracts.*.credentials",
                        &schema.value_type,
                        schema.required,
                        &schema.default,
                    )?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        identity_exports: spec
            .identity_exports
            .iter()
            .map(|(name, value)| {
                Ok((
                    identifier("contracts.*.identity_exports", name)?,
                    value.clone(),
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        runtime_exports: spec
            .runtime_exports
            .iter()
            .map(|(name, export)| {
                // Shorthand and detailed spellings canonicalize into one
                // shape (shorthand ⇒ secret: false).
                let normalized = match export {
                    RuntimeExportSpec::Shorthand(value) => NormalizedRuntimeExportIntent {
                        value: value.clone(),
                        secret: false,
                    },
                    RuntimeExportSpec::Detailed(detailed) => NormalizedRuntimeExportIntent {
                        value: detailed.value.clone(),
                        secret: detailed.secret,
                    },
                };
                Ok((identifier("contracts.*.runtime_exports", name)?, normalized))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        state: spec
            .state
            .as_ref()
            .map(|state| {
                Ok::<_, CapsuleProgramError>(NormalizedContractStateIntent {
                    required: state.required,
                    version: state
                        .version
                        .as_deref()
                        .map(|version| authored("contracts.*.state.version", version))
                        .transpose()?,
                    mount: state
                        .mount
                        .as_deref()
                        .map(|mount| guest_path("contracts.*.state.mount", mount))
                        .transpose()?,
                })
            })
            .transpose()?,
    })
}

fn host_capabilities_intent(
    capabilities: &[HostCapabilitySpec],
) -> Result<Vec<NormalizedHostCapabilityIntent>, CapsuleProgramError> {
    let mut entries = capabilities
        .iter()
        .map(|spec| {
            Ok(NormalizedHostCapabilityIntent {
                name: serde_identifier("host_capabilities.name", &spec.name)?,
                reason: authored("host_capabilities.reason", &spec.reason)?,
            })
        })
        .collect::<Result<Vec<_>, CapsuleProgramError>>()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(invalid_value(
            "host_capabilities",
            "duplicate capability name (ambiguous authoring, fail closed)",
        ));
    }
    Ok(entries)
}

fn ingress_intent(ingress: &IngressConfig) -> Result<NormalizedIngressIntent, CapsuleProgramError> {
    Ok(NormalizedIngressIntent {
        mode: serde_identifier("ingress.mode", &ingress.mode)?,
        routes: ingress
            .routes
            .iter()
            .map(|(name, route)| {
                Ok((
                    identifier("ingress.routes", name)?,
                    NormalizedIngressRouteIntent {
                        target: identifier("ingress.routes.*.target", &route.target)?,
                        port: route.port,
                        listed: route.listed,
                        alias: route
                            .alias
                            .as_deref()
                            .map(|alias| identifier("ingress.routes.*.alias", alias))
                            .transpose()?,
                        strip_prefix: route.strip_prefix,
                        upstream_path_prefix: route
                            .upstream_path_prefix
                            .as_deref()
                            .map(|prefix| {
                                http_target("ingress.routes.*.upstream_path_prefix", prefix)
                            })
                            .transpose()?,
                        root: route.root,
                    },
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
        env_inject: ingress
            .env_inject
            .iter()
            .map(|(service, env)| {
                Ok((
                    identifier("ingress.env_inject", service)?,
                    env.iter()
                        .map(|(name, value)| {
                            Ok((
                                identifier("ingress.env_inject", name)?,
                                authored("ingress.env_inject", value)?,
                            ))
                        })
                        .collect::<Result<BTreeMap<_, _>, CapsuleProgramError>>()?,
                ))
            })
            .collect::<Result<_, CapsuleProgramError>>()?,
    })
}

fn snapshot_intent(
    snapshot: &SnapshotConfig,
) -> Result<Option<NormalizedSnapshotIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedSnapshotIntent {
        mode: non_default_identifier(
            "snapshot.mode",
            &snapshot.mode,
            &crate::types::SnapshotMode::default(),
        )?,
        boot_until: non_default_identifier(
            "snapshot.boot_until",
            &snapshot.boot_until,
            &crate::types::BootUntil::default(),
        )?,
        sanitize_after_restore: snapshot.sanitize_after_restore,
        runner_class: snapshot
            .runner_class
            .as_deref()
            .map(|class| identifier("snapshot.runner_class", class))
            .transpose()?,
        max_restore_seconds: snapshot.max_restore_seconds,
        warmup_paths: snapshot
            .warmup_paths
            .iter()
            .map(|path| http_target("snapshot.warmup_paths", path))
            .collect::<Result<_, _>>()?,
        stable_successes: (snapshot.stable_successes != 1).then_some(snapshot.stable_successes),
        stable_interval_ms: (snapshot.stable_interval_ms != 250)
            .then_some(snapshot.stable_interval_ms),
        content_ready_path: snapshot
            .content_ready_path
            .as_deref()
            .map(|path| http_target("snapshot.content_ready_path", path))
            .transpose()?,
    })
}

fn secret_intent(spec: &SecretSpec) -> Result<NormalizedSecretIntent, CapsuleProgramError> {
    Ok(NormalizedSecretIntent {
        required: spec.required,
        description: spec
            .description
            .as_deref()
            .map(|description| authored("secrets.*.description", description))
            .transpose()?,
        env: spec
            .env
            .as_deref()
            .map(|env| identifier("secrets.*.env", env))
            .transpose()?,
        delivery: non_default_identifier(
            "secrets.*.delivery",
            &spec.delivery,
            &crate::types::SecretDelivery::default(),
        )?,
        class: non_default_identifier(
            "secrets.*.class",
            &spec.class,
            &crate::types::SecretClass::default(),
        )?,
    })
}

fn binding_intent(
    spec: &crate::types::BindingSpec,
) -> Result<NormalizedBindingIntent, CapsuleProgramError> {
    Ok(NormalizedBindingIntent {
        kind: serde_identifier("bindings.*.kind", &spec.kind)?,
        required: spec.required,
        scope: non_default_identifier(
            "bindings.*.scope",
            &spec.scope,
            &crate::types::BindingScope::default(),
        )?,
        mount: spec
            .mount
            .as_deref()
            .map(|mount| guest_path("bindings.*.mount", mount))
            .transpose()?,
        mode: spec
            .mode
            .as_ref()
            .map(|mode| serde_identifier("bindings.*.mode", mode))
            .transpose()?,
        provider: spec
            .provider
            .as_deref()
            .map(|provider| identifier("bindings.*.provider", provider))
            .transpose()?,
    })
}

fn external_intent(
    spec: &ExternalCapabilitySpec,
) -> Result<NormalizedExternalIntent, CapsuleProgramError> {
    Ok(NormalizedExternalIntent {
        kind: identifier("external.*.type", &spec.kind)?,
        required: spec.required,
        providers: spec
            .providers
            .iter()
            .map(|provider| identifier("external.*.providers", provider))
            .collect::<Result<_, _>>()?,
        provider: spec
            .provider
            .as_deref()
            .map(|provider| identifier("external.*.provider", provider))
            .transpose()?,
        provision: non_default_identifier(
            "external.*.provision",
            &spec.provision,
            &crate::types::ProvisionMode::default(),
        )?,
        locality: non_default_identifier(
            "external.*.locality",
            &spec.locality,
            &crate::types::Locality::default(),
        )?,
        degraded: non_default_identifier(
            "external.*.degraded",
            &spec.degraded,
            &crate::types::DegradedMode::default(),
        )?,
    })
}

fn context_intent(
    context: &ContextConfig,
) -> Result<Option<NormalizedContextIntent>, CapsuleProgramError> {
    omit_if_default(NormalizedContextIntent {
        store: non_default_identifier(
            "context.store",
            &context.store,
            &crate::types::ContextStore::default(),
        )?,
        artifacts: context.artifacts,
        index: context.index,
        mount: context
            .mount
            .as_deref()
            .map(|mount| guest_path("context.mount", mount))
            .transpose()?,
        provenance: context.provenance,
    })
}

fn generated_binding_intent(
    spec: &GeneratedBindingSpec,
) -> Result<NormalizedGeneratedBindingIntent, CapsuleProgramError> {
    Ok(NormalizedGeneratedBindingIntent {
        generator: non_default_identifier(
            "generated_bindings.*.generator",
            &spec.generator,
            &crate::types::GeneratedGenerator::default(),
        )?,
        bytes: (spec.bytes != 32).then_some(spec.bytes),
        scope: non_default_identifier(
            "generated_bindings.*.scope",
            &spec.scope,
            &crate::types::GeneratedBindingScope::default(),
        )?,
        targets: identifier_set("generated_bindings.*.targets", &spec.targets)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(toml_text: &str) -> CapsuleManifest {
        CapsuleManifest::from_toml(toml_text).expect("model parses")
    }

    fn intent(toml_text: &str, root: &Path) -> ProgramManifestIntentV1 {
        program_intent_from_v03(&model(toml_text), toml_text, root).expect("intent")
    }

    fn intent_err(toml_text: &str, root: &Path) -> CapsuleProgramError {
        program_intent_from_v03(&model(toml_text), toml_text, root).expect_err("must reject")
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn ir_json(intent: &ProgramManifestIntentV1) -> String {
        serde_json::to_string(intent).expect("serializable")
    }

    const BASE: &str = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

    // ── conformance (ADR-014 §2.0.1 / §9) ────────────────────────────────

    // The v0.3 TOML normalizer forbids the raw `entrypoint` spelling in
    // target tables (legacy-field rule) — a populated `entrypoint` only
    // arises post-normalization or from machine-generated (JSON) models, so
    // the canonicalization vectors below exercise the adapter seam with
    // constructed models rather than raw TOML.
    #[test]
    fn structured_source_target_and_equivalent_named_target_canonicalize_identically() {
        let root = tmp();
        let structured = source_target_intent(
            &SourceTarget {
                language: "python".to_string(),
                version: None,
                entrypoint: "main.py".to_string(),
                dependencies: None,
                args: Vec::new(),
                dev_mode: false,
            },
            root.path(),
        )
        .expect("structured source target");
        let named = named_target_intent(
            &NamedTarget {
                runtime: "source".to_string(),
                language: Some("python".to_string()),
                entrypoint: "main.py".to_string(),
                ..NamedTarget::default()
            },
            root.path(),
        )
        .expect("named source target");
        assert_eq!(
            serde_json::to_string(&structured).unwrap(),
            serde_json::to_string(&named).unwrap(),
            "structured and named spellings of the same target intent must be byte-identical"
        );
    }

    #[test]
    fn excluded_top_level_fields_never_change_the_ir() {
        let variant = r#"
schema_version = "0.3"
name = "renamed-fixture"
version = "9.9.9"
type = "app"
default_target = "app"

[metadata]
display_name = "Renamed"
description = "different display metadata"

[routing]
weight = "heavy"

[pool]
enabled = true

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;
        let root = tmp();
        assert_eq!(
            ir_json(&intent(BASE, root.path())),
            ir_json(&intent(variant, root.path())),
            "name/version/metadata/routing/pool are non-identity"
        );
    }

    #[test]
    fn service_command_alias_and_target_run_alias_normalize_to_the_same_ir() {
        let with_alias = r#"
schema_version = "0.3"
name = "alias-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run = "node server.js"

[services.main]
command = "node server.js"
"#;
        let canonical = r#"
schema_version = "0.3"
name = "alias-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"

[services.main]
entrypoint = "node server.js"
"#;
        let root = tmp();
        assert_eq!(
            ir_json(&intent(with_alias, root.path())),
            ir_json(&intent(canonical, root.path()))
        );
    }

    #[test]
    fn explicit_default_sections_collapse_to_the_absent_spelling() {
        let with_defaults =
            format!("{BASE}\n[snapshot]\nmode = \"none\"\n\n[network]\n\n[pack]\n\n[context]\n");
        let root = tmp();
        assert_eq!(
            ir_json(&intent(BASE, root.path())),
            ir_json(&intent(&with_defaults, root.path())),
            "authored explicit defaults must not change the IR"
        );
    }

    // ── strict gate rejections ───────────────────────────────────────────

    #[test]
    fn unknown_top_level_key_is_rejected_naming_the_key() {
        let manifest = format!("{BASE}\ndescription = \"marketing copy\"\n");
        let error = intent_err(&manifest, tmp().path());
        match error {
            CapsuleProgramError::ManifestInput(message) => {
                assert!(message.contains("description"), "{message}");
            }
            other => panic!("expected ManifestInput, got {other:?}"),
        }
    }

    #[test]
    fn unknown_key_inside_a_named_target_is_rejected_naming_key_and_label() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
bogus_key = 1
"#;
        let error = program_intent_from_v03(&model(BASE), manifest, tmp().path())
            .expect_err("unknown named-target key must fail closed");
        match error {
            CapsuleProgramError::ManifestInput(message) => {
                assert!(message.contains("bogus_key"), "{message}");
                assert!(message.contains("targets.app"), "{message}");
            }
            other => panic!("expected ManifestInput, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_reserved_targets_key_is_rejected() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
type = "app"

[targets]
port = 1
port = 2
"#;
        assert!(matches!(
            parse_program_manifest_v03_input(manifest),
            Err(CapsuleProgramError::ManifestInput(_))
        ));
    }

    #[test]
    fn workspace_fails_closed_with_the_typed_error() {
        let manifest = format!("{BASE}\n[workspace]\ndefault_app = \"app\"\n");
        assert_eq!(
            intent_err(&manifest, tmp().path()),
            CapsuleProgramError::UnsupportedField("workspace")
        );
    }

    #[test]
    fn engine_path_fails_closed() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
engine_path = "/usr/local/bin/llama-server"
"#;
        assert_eq!(
            intent_err(manifest, tmp().path()),
            CapsuleProgramError::UnsupportedField("engine_path")
        );
    }

    #[test]
    fn wasm_working_dir_fails_closed() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "wasm"
component = "app.wasm"
working_dir = "srv"
"#;
        assert_eq!(
            intent_err(manifest, tmp().path()),
            CapsuleProgramError::UnsupportedField("wasm working_dir")
        );
    }

    // ── SourceExistingPath policy (targets.*.model) ──────────────────────

    #[test]
    fn model_path_must_be_relative_and_existing() {
        let template = |path: &str| {
            format!(
                r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
model = "{path}"
"#
            )
        };
        let root = tmp();
        std::fs::write(root.path().join("model.gguf"), b"gguf").expect("write model");

        assert!(matches!(
            intent_err(&template("/opt/models/model.gguf"), root.path()),
            CapsuleProgramError::InvalidValue {
                field: "targets.*.model",
                ..
            }
        ));
        assert!(matches!(
            intent_err(&template("missing.gguf"), root.path()),
            CapsuleProgramError::InvalidValue {
                field: "targets.*.model",
                ..
            }
        ));

        let accepted = intent(&template("model.gguf"), root.path());
        let target =
            &accepted.targets.as_ref().unwrap().targets[&ProgramIdentifier::parse("chat").unwrap()];
        assert_eq!(
            serde_json::to_value(target.model.as_ref().unwrap()).unwrap(),
            serde_json::json!("model.gguf")
        );
    }

    // ── working_dir 3-way split / semantic types ─────────────────────────

    #[test]
    fn working_dir_resolves_by_runtime_class() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
working_dir = "/app"

[targets.builder]
runtime = "source"
run = "python main.py"
working_dir = "packages/app"
"#;
        let root = tmp();
        let ir = intent(manifest, root.path());
        let targets = &ir.targets.as_ref().unwrap().targets;
        assert_eq!(
            serde_json::to_value(
                targets[&ProgramIdentifier::parse("app").unwrap()]
                    .working_dir
                    .as_ref()
                    .unwrap()
            )
            .unwrap(),
            serde_json::json!({"guest": "/app"})
        );
        assert_eq!(
            serde_json::to_value(
                targets[&ProgramIdentifier::parse("builder").unwrap()]
                    .working_dir
                    .as_ref()
                    .unwrap()
            )
            .unwrap(),
            serde_json::json!({"source_relative": "packages/app"})
        );
    }

    // The existing normalizer legitimately produces `"."` for a web static
    // root entrypoint (ADR-014 r6) — exercised at the adapter seam because
    // the raw `entrypoint` spelling never survives v0.3 TOML normalization.
    #[test]
    fn web_root_entrypoint_canonicalizes_as_root() {
        let root = tmp();
        let target = named_target_intent(
            &NamedTarget {
                runtime: "web".to_string(),
                entrypoint: ".".to_string(),
                ..NamedTarget::default()
            },
            root.path(),
        )
        .expect("web root entrypoint");
        assert_eq!(
            serde_json::to_value(target.entrypoint.as_ref().unwrap()).unwrap(),
            serde_json::json!(".")
        );
    }

    #[test]
    fn probe_port_reference_and_tcp_target_never_interchange() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"

[targets.app.readiness_probe]
tcp_connect = "db:5432"
port = "web"
"#;
        let root = tmp();
        let ir = intent(manifest, root.path());
        let probe = ir.targets.as_ref().unwrap().targets[&ProgramIdentifier::parse("app").unwrap()]
            .readiness_probe
            .as_ref()
            .unwrap();
        assert_eq!(probe.port.as_ref().unwrap().as_str(), "web");
        assert_eq!(probe.tcp_connect.as_ref().unwrap().as_str(), "db:5432");
    }

    #[test]
    fn model_sha256_spellings_normalize_into_one_ir_and_source_digest_requires_prefix() {
        let bare = "ab".repeat(32);
        let template = |pin: &str| {
            format!(
                r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
model_url = "https://example.invalid/model.gguf"
model_sha256 = "{pin}"
"#
            )
        };
        let root = tmp();
        assert_eq!(
            ir_json(&intent(&template(&bare), root.path())),
            ir_json(&intent(&template(&format!("sha256:{bare}")), root.path())),
            "both authoring spellings must produce the SAME IR"
        );

        let source_digest = |digest: &str| {
            format!(
                r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets]
source_digest = "{digest}"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
"#
            )
        };
        assert!(matches!(
            intent_err(&source_digest(&bare), root.path()),
            CapsuleProgramError::InvalidValue {
                field: "targets.source_digest",
                ..
            }
        ));
        let prefixed = intent(&source_digest(&format!("sha256:{bare}")), root.path());
        assert_eq!(
            serde_json::to_value(prefixed.targets.as_ref().unwrap().source_digest.unwrap())
                .unwrap(),
            serde_json::json!(bare),
            "canonical IR spelling is bare lowercase hex"
        );
    }

    #[test]
    fn wasm_world_defaults_and_oci_user_spec_are_canonicalized() {
        let manifest = format!(
            r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "wasm"

[targets.wasm]
digest = "sha256:{digest}"

[targets.oci]
image = "ghcr.io/example/app:1"
user = "1000:1000"
"#,
            digest = "0c".repeat(32)
        );
        let root = tmp();
        let ir = intent(&manifest, root.path());
        let targets = &ir.targets.as_ref().unwrap().targets;
        assert_eq!(
            targets[&ProgramIdentifier::parse("wasm").unwrap()]
                .world
                .as_ref()
                .unwrap()
                .as_str(),
            "wasi:cli/command",
            "authored-absent world is default-expanded before hashing"
        );
        assert_eq!(
            targets[&ProgramIdentifier::parse("oci").unwrap()]
                .user
                .as_ref()
                .unwrap()
                .as_str(),
            "1000:1000"
        );
    }

    #[test]
    fn structured_target_colliding_with_a_named_label_fails_closed() {
        let manifest = r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "oci"

[targets.oci]
image = "ghcr.io/example/app:1"
"#;
        let mut collided = model(manifest);
        collided
            .targets
            .as_mut()
            .unwrap()
            .named
            .insert("oci".to_string(), NamedTarget::default());
        assert!(matches!(
            program_intent_from_v03(&collided, manifest, tmp().path()),
            Err(CapsuleProgramError::InvalidValue {
                field: "targets",
                ..
            })
        ));
    }

    // ── real-manifest smoke (repo fixtures / samples, inlined) ───────────

    fn loaded_intent(manifest: &str) -> ProgramManifestIntentV1 {
        let root = tmp();
        let path = root.path().join("capsule.toml");
        std::fs::write(&path, manifest).expect("write manifest");
        let loaded = crate::manifest::load_manifest(&path).expect("load_manifest");
        program_intent_from_v03(&loaded.model, &loaded.raw_text, root.path())
            .expect("real manifest must produce an intent")
    }

    /// Verbatim copy of `tests/fixtures/local-install/basic-web/capsule.toml`
    /// (source runtime + pack + metadata; every top-level key classified).
    #[test]
    fn real_manifest_basic_web_smoke() {
        let manifest = r#"
schema_version = "0.3"
name = "basic-web"
version = "0.1.0"
type = "app"
default_target = "main"

[metadata]
display_name = "Basic Web (local fixture)"
description = "Deterministic no-network local fixture: serves a known HTTP response."

[targets.main]
runtime = "source"
driver = "node"
runtime_version = "22.14.0"
run = "node server.js"
port = 18890

[pack]
include = [
  "capsule.toml",
  "package.json",
  "package-lock.json",
  "server.js",
  "README.md",
]
"#;
        let ir = loaded_intent(manifest);
        assert_eq!(ir.capsule_type.as_str(), "app");
        assert_eq!(ir.default_target.as_ref().unwrap().as_str(), "main");
        assert_eq!(ir.pack.as_ref().unwrap().include.len(), 5);
        let target =
            &ir.targets.as_ref().unwrap().targets[&ProgramIdentifier::parse("main").unwrap()];
        assert_eq!(
            target.run_command.as_ref().unwrap().as_str(),
            "node server.js"
        );
        assert_eq!(target.port, Some(18890));
    }

    /// Verbatim copy of
    /// `tests/fixtures/local-install/launch-conditions/capsule.toml`
    /// (OCI target + explicit-attach state + service state_bindings).
    #[test]
    fn real_manifest_launch_conditions_smoke() {
        let manifest = r#"
schema_version = "0.3"
name = "launch-conditions"
version = "0.1.0"
type = "app"
default_target = "app"

[metadata]
display_name = "Launch Conditions (local fixture)"
description = "Declares an explicit-attach state binding + a fixed port."

[targets.app]
runtime = "oci"
image = "docker.io/library/busybox:latest"
port = 18891

[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"
attach = "explicit"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app/data"

[pack]
include = ["capsule.toml", "README.md"]
"#;
        let ir = loaded_intent(manifest);
        let state = &ir.state[&ProgramIdentifier::parse("data").unwrap()];
        assert_eq!(state.durability.as_str(), "ephemeral");
        assert_eq!(state.attach.as_ref().unwrap().as_str(), "explicit");
        let service = &ir.services[&ProgramIdentifier::parse("main").unwrap()];
        assert_eq!(service.state_bindings.len(), 1);
        assert_eq!(
            service.state_bindings[0].target.as_str(),
            "/var/lib/app/data"
        );
    }

    /// Representative copy of `samples/local-llm-chat/capsule.toml`
    /// (native-inference targets: managed engine + pinned model). The real
    /// file's top-level `description` / `homepage` keys are NOT
    /// `CapsuleManifest` fields — ADR-014 §2.1 is exhaustive, so the strict
    /// gate rejects them (see
    /// `unclassified_recipe_keys_are_rejected_fail_closed`) and they are
    /// omitted from this copy.
    #[test]
    fn real_manifest_local_llm_chat_smoke() {
        let manifest = r#"
schema_version = "0.3"
name = "local-llm-chat"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
engine_version = "b9754"
model_url = "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
model_sha256 = "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e"
model_format = "gguf"
port = 8080

[targets.chat-vulkan]
runtime = "native-inference"
engine = "llama.cpp"
engine_version = "b9754"
engine_variant = "vulkan"
model_url = "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf"
model_sha256 = "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e"
model_format = "gguf"
port = 8080
"#;
        let ir = loaded_intent(manifest);
        let targets = &ir.targets.as_ref().unwrap().targets;
        let chat = &targets[&ProgramIdentifier::parse("chat").unwrap()];
        assert_eq!(
            chat.model_sha256.unwrap().to_string(),
            "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e"
        );
        // model_format is a Rule-2 exclusion: it must not reach the IR.
        assert!(!ir_json(&ir).contains("model_format"));
        let vulkan = &targets[&ProgramIdentifier::parse("chat-vulkan").unwrap()];
        assert_eq!(vulkan.engine_variant.as_ref().unwrap().as_str(), "vulkan");
    }

    /// Representative copy of `samples/recipes/pgweb/capsule.toml` (OCI +
    /// services readiness probe), minus the corpus keys the ADR classifies as
    /// unknown (`description`, `homepage`, `[source]`).
    #[test]
    fn real_manifest_pgweb_style_smoke() {
        let manifest = r#"
schema_version = "0.3"
name = "pgweb"
version = "0.17.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "sosedoff/pgweb:0.17.0"
port = 8081

[services.main]
target = "app"
readiness_probe = { http_get = "/", port = "8081" }
"#;
        let ir = loaded_intent(manifest);
        let service = &ir.services[&ProgramIdentifier::parse("main").unwrap()];
        let probe = service.readiness_probe.as_ref().unwrap();
        assert_eq!(probe.http_get.as_ref().unwrap().as_str(), "/");
        assert_eq!(probe.port.as_ref().unwrap().as_str(), "8081");
    }

    /// The published recipe corpus commonly carries top-level `description`,
    /// `homepage`, and `[source]` keys that are NOT `CapsuleManifest` fields
    /// (the tolerant model parser silently drops them). ADR-014 §2.1's
    /// classification is exhaustive, so Program Identity issuance fails
    /// closed on them rather than hashing around silently-dropped authoring.
    #[test]
    fn unclassified_recipe_keys_are_rejected_fail_closed() {
        for extra in [
            "description = \"Web-based client\"",
            "homepage = \"https://example.invalid\"",
            "[source]\nrepository = \"example/app\"",
        ] {
            let manifest = format!("{BASE}\n{extra}\n");
            assert!(
                matches!(
                    intent_err(&manifest, tmp().path()),
                    CapsuleProgramError::ManifestInput(_)
                ),
                "{extra} must fail the strict gate"
            );
        }
    }

    #[test]
    fn adapter_output_passes_ir_validation_and_is_deterministic() {
        let root = tmp();
        let first = intent(BASE, root.path());
        first.validate().expect("adapter output is canonical");
        assert_eq!(ir_json(&first), ir_json(&intent(BASE, root.path())));
    }
}
