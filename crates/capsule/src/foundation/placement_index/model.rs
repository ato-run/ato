//! Redacted data model for the cross-device placement index (#509).
//!
//! Every type here is **redacted by construction**: there is deliberately no
//! field in which a secret *value* or a raw sensitive local *path* can be
//! stored. The index trades in references and summaries only —
//! [`RedactedSecretRef`] carries a secret's reference *name*, never its value;
//! [`MaterializedObjectSummary`] carries content hashes and counts, never
//! local cache paths. Projecting an actual secret value or resolving a local
//! path is provider-local work that happens later (#501 projection, #508
//! installed-state DB), never in this index.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Identifier newtypes
// ---------------------------------------------------------------------------

/// Opaque identifier for a device participating in placement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub String);

impl DeviceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identifier for a provider (a realization surface on a device).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque provider-capability identifier.
///
/// The final capability vocabulary is owned by #501 (provider projection
/// boundary). Until then these are deliberately opaque strings so #509 does
/// not invent a vocabulary #501 would have to unwind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProviderCapabilityId(pub String);

impl ProviderCapabilityId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderCapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A **redacted** reference to a secret.
///
/// Holds only the secret's reference *name* (e.g. `"OPENAI_API_KEY"`), never a
/// resolved value. There is no constructor and no field that accepts a value;
/// the type exists precisely so the placement index cannot accidentally carry
/// secret material. Actual projection of the value is provider-local (#501).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RedactedSecretRef(String);

impl RedactedSecretRef {
    /// Construct from a secret reference *name* (e.g. `"OPENAI_API_KEY"`).
    ///
    /// Callers must pass the reference name, never a resolved value. The index
    /// stores and compares only the reference.
    pub fn new(reference_name: impl Into<String>) -> Self {
        Self(reference_name.into())
    }

    /// The secret's reference name. Never a value.
    pub fn reference_name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RedactedSecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Snapshot value types
// ---------------------------------------------------------------------------

/// Whether a provider can realize (run) capsules or is only a control surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRole {
    /// Can realize/run capsules — a valid realization candidate.
    Realizer,
    /// Can only drive other devices (e.g. a phone). Never a realization
    /// target, even when online and otherwise capable.
    ControlSurfaceOnly,
}

/// Liveness of a provider as last observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnlineStatus {
    Online,
    Offline,
    Unknown,
}

/// GPU vendor family. Coarse on purpose — fine-grained model strings are not
/// needed for candidate filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Apple,
    Intel,
    Other,
}

/// Coarse provider kind. Informational; filtering keys on [`DeviceRole`] and
/// the resource/runtime summaries, not on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    DesktopWorkstation,
    CloudGpu,
    HomeDesktop,
    Mobile,
    Server,
    Other,
}

/// OS / arch summary. Plain platform identifiers, no host paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformSummary {
    /// e.g. `"darwin"`, `"linux"`, `"windows"`.
    pub os: String,
    /// e.g. `"arm64"`, `"x86_64"`.
    pub arch: String,
}

/// GPU capability summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuSummary {
    pub vendor: GpuVendor,
    pub vram_bytes: u64,
    /// CUDA toolkit version string (e.g. `"12.4"`) when applicable.
    pub cuda_version: Option<String>,
}

/// Coarse resource summary. Storage is summarized as available bytes; no
/// device path, mount point, or filesystem layout is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSummary {
    pub available_storage_bytes: u64,
    pub total_memory_bytes: u64,
    pub gpu: Option<GpuSummary>,
}

/// Runtime families available on the provider (e.g. `"node"`, `"python"`,
/// `"oci"`). Opaque family strings; no install paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSummary {
    pub families: Vec<String>,
}

/// Network egress capability summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCapabilitySummary {
    /// Host patterns the provider is allowed to egress to (e.g.
    /// `"api.openai.com"`).
    pub egress_allowed: Vec<String>,
    /// When true, the provider can reach any host (egress is unrestricted).
    pub egress_unrestricted: bool,
}

/// Summary of objects already materialized on the provider.
///
/// **Hashes and counts only.** Local cache paths are never stored here; if a
/// path is needed it belongs in the provider-local #508 DB, not in this index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializedObjectSummary {
    /// Content hashes of materialized objects present on the provider.
    pub object_hashes: Vec<String>,
    pub object_count: u64,
    pub total_bytes: u64,
}

/// Redacted summary of a secret the provider can project.
///
/// Carries the redacted reference and whether projection is possible — never
/// the value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretProjectionSummary {
    pub secret_ref: RedactedSecretRef,
    /// Projection scope label (e.g. `"project"`, `"user"`). Not a value.
    pub scope: String,
    pub can_project: bool,
}

/// Soft placement hints used only for deterministic ordering, never for
/// eligibility.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementHints {
    pub estimated_latency_ms: Option<u64>,
    pub estimated_cost_milli_units: Option<u64>,
}

/// A redacted snapshot of one provider's capabilities, as advertised to the
/// cross-device placement index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilitySnapshot {
    pub device_id: DeviceId,
    pub provider_id: ProviderId,
    pub provider_kind: ProviderKind,
    pub role: DeviceRole,
    pub online_status: OnlineStatus,
    pub last_seen_unix_ms: u64,
    pub platform: PlatformSummary,
    pub resources: ResourceSummary,
    pub runtimes: RuntimeSummary,
    pub network: NetworkCapabilitySummary,
    pub capabilities: Vec<ProviderCapabilityId>,
    pub materialized_objects: MaterializedObjectSummary,
    pub secret_refs: Vec<SecretProjectionSummary>,
    pub placement_hints: PlacementHints,
}

// ---------------------------------------------------------------------------
// Placement request
// ---------------------------------------------------------------------------

/// A required runtime family. Kept minimal for this slice (family only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRequirement {
    pub family: String,
}

impl RuntimeRequirement {
    pub fn new(family: impl Into<String>) -> Self {
        Self {
            family: family.into(),
        }
    }
}

/// A required GPU capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuRequirement {
    /// Require an NVIDIA GPU specifically (CUDA workloads).
    pub require_nvidia: bool,
    pub min_vram_bytes: u64,
    /// Minimum CUDA version string (e.g. `"12"`), when required.
    pub min_cuda_version: Option<String>,
}

/// A required network egress destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRequirement {
    pub host: String,
}

impl NetworkRequirement {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }
}

/// What a capsule needs from a provider, expressed as redacted requirements.
///
/// Deliberately independent from `capsule.toml` parsing for this slice; later
/// PRs map the Capsule Execution Contract / #503 / #508 data into this shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlacementRequest {
    pub requested_capsule: String,
    pub required_storage_bytes: Option<u64>,
    pub required_runtimes: Vec<RuntimeRequirement>,
    pub required_gpu: Option<GpuRequirement>,
    pub required_network: Vec<NetworkRequirement>,
    pub required_secret_refs: Vec<RedactedSecretRef>,
    pub required_provider_capabilities: Vec<ProviderCapabilityId>,
    /// Hard requirement: the provider must already hold these object hashes.
    pub required_materialized_objects: Vec<String>,
    /// Soft preference: providers that already hold these object hashes are
    /// ranked ahead of equally-eligible ones (data locality). Never a
    /// rejection reason, and expressed as hashes — never paths.
    pub preferred_materialized_objects: Vec<String>,
}

// ---------------------------------------------------------------------------
// Typed rejection reasons
// ---------------------------------------------------------------------------

/// Why a provider was rejected as a placement candidate. Typed so callers can
/// branch on the reason; the human string is only a projection (see
/// [`std::fmt::Display`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementRejectionReason {
    StaleSnapshot,
    Offline,
    ControlSurfaceOnly,
    MissingRuntime {
        runtime: String,
    },
    InsufficientStorage {
        required_bytes: u64,
        available_bytes: u64,
    },
    MissingGpu,
    InsufficientGpuVram {
        required_bytes: u64,
        available_bytes: u64,
    },
    CudaVersionTooLow {
        required: String,
        available: Option<String>,
    },
    MissingNetworkCapability {
        requirement: String,
    },
    MissingSecretProjection {
        secret_ref: RedactedSecretRef,
    },
    MissingProviderCapability {
        capability: ProviderCapabilityId,
    },
    MissingMaterializedObject {
        hash: String,
    },
}

impl std::fmt::Display for PlacementRejectionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleSnapshot => write!(f, "snapshot is stale (last-seen older than TTL)"),
            Self::Offline => write!(f, "provider is offline"),
            Self::ControlSurfaceOnly => {
                write!(
                    f,
                    "provider is control-surface-only and cannot realize capsules"
                )
            }
            Self::MissingRuntime { runtime } => write!(f, "missing runtime family: {runtime}"),
            Self::InsufficientStorage {
                required_bytes,
                available_bytes,
            } => write!(
                f,
                "insufficient storage: need {required_bytes} bytes, have {available_bytes}"
            ),
            Self::MissingGpu => write!(f, "no suitable GPU"),
            Self::InsufficientGpuVram {
                required_bytes,
                available_bytes,
            } => write!(
                f,
                "insufficient GPU VRAM: need {required_bytes} bytes, have {available_bytes}"
            ),
            Self::CudaVersionTooLow {
                required,
                available,
            } => match available {
                Some(av) => write!(f, "CUDA too low: need >= {required}, have {av}"),
                None => write!(f, "CUDA too low: need >= {required}, none reported"),
            },
            Self::MissingNetworkCapability { requirement } => {
                write!(f, "missing network egress capability: {requirement}")
            }
            Self::MissingSecretProjection { secret_ref } => {
                write!(f, "cannot project required secret ref: {secret_ref}")
            }
            Self::MissingProviderCapability { capability } => {
                write!(f, "missing provider capability: {capability}")
            }
            Self::MissingMaterializedObject { hash } => {
                write!(f, "missing required materialized object: {hash}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Query result + decision receipt
// ---------------------------------------------------------------------------

/// A projection the selected provider must perform locally before/at
/// realization. Records references only — never values or paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequiredProjection {
    /// A secret reference that must be projected provider-locally (#501).
    Secret(RedactedSecretRef),
    /// A materialized object (by content hash) the provider must have/fetch.
    MaterializedObject { hash: String },
}

/// An eligible placement candidate.
///
/// **Non-authoritative.** A candidate is a *narrowing* result, not an
/// admission: [`Self::requires_final_local_admission`] is always `true`. The
/// selected provider's local installed-state DB performs the real
/// admission/reservation later (#508).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementCandidate {
    pub provider_id: ProviderId,
    pub device_id: DeviceId,
    /// Human-readable strengths that made this an eligible candidate.
    pub selected_reason: Vec<String>,
    pub required_projections: Vec<RequiredProjection>,
    /// Always `true`: the index never admits. See the type docs.
    pub requires_final_local_admission: bool,
}

/// A rejected candidate plus its typed reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedPlacementCandidate {
    pub provider_id: ProviderId,
    pub device_id: DeviceId,
    pub reasons: Vec<PlacementRejectionReason>,
}

/// The result of a placement query: eligible candidates (deterministically
/// ordered, best first) and rejected candidates with typed reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementQueryResult {
    pub eligible: Vec<PlacementCandidate>,
    pub rejected: Vec<RejectedPlacementCandidate>,
}

/// A placement *decision* artifact. **Not** the full execution receipt — it
/// records only the cross-device narrowing decision and makes the two-phase
/// contract explicit via [`Self::requires_final_local_admission`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementDecisionReceipt {
    pub requested_capsule: String,
    pub selected_provider: Option<ProviderId>,
    pub rejected_candidates: Vec<RejectedPlacementCandidate>,
    pub selected_reason: Vec<String>,
    pub required_projections: Vec<RequiredProjection>,
    /// `true` whenever a provider was selected: the cross-device index only
    /// narrows candidates; the selected provider still requires final
    /// provider-local admission/reservation (#508) before realization.
    pub requires_final_local_admission: bool,
}
