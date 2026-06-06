//! Capsule Interface Model (#505).
//!
//! This module introduces a typed `provides` / `requires` model for Capsules.
//! It treats a Capsule not merely as a `target` / `run` definition but as an
//! **application-owned execution contract**:
//!
//! ```text
//! CapsuleInterface:
//!   provides: what this Capsule exposes to the outside
//!   requires: what this Capsule needs in order to launch and run
//! ```
//!
//! ## Scope boundary
//!
//! This is the *type* foundation only. It deliberately does **not** implement
//! composition (#506), placement, install-flow admission (#508/#509),
//! secret-store integration, provider-capability detection, or actual port
//! allocation. It defines the types, normalization, validation, and a single
//! pure satisfaction predicate that #506 can build a composition / aggregate
//! execution contract on top of.
//!
//! ## The key design rule
//!
//! A [`RequiredInterface`] is **not** satisfied only by another Capsule's
//! [`ProvidedInterface`]. A requirement may be satisfied by any of:
//!
//! ```text
//! RequiredInterface may be satisfied by:
//!   - another Capsule's ProvidedInterface   (peer wiring: service/runtime/tool/state)
//!   - a user secret grant                    (Secret/Auth)
//!   - a provider capability                  (ProviderCapability / Capability policy)
//!   - a managed runtime / resource           (GPU and other hardware, network egress)
//!   - a state binding                        (State)
//!   - a port allocation / remap              (Port)
//! ```
//!
//! In particular the following are **never** satisfied by a peer Capsule's
//! `provides` — they come from the host / platform / user, not from wiring two
//! Capsules together:
//!
//! ```text
//! Secret / Auth
//! GPU (and hardware in general)
//! network egress
//! port availability
//! provider capability
//! local disk
//! ```
//!
//! Getting this boundary right is what keeps #506 composition from collapsing
//! into a Docker-Compose-style service graph. See [`provided_interface_may_satisfy`]
//! and [`SatisfactionSource`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Version constraint ─────────────────────────────────────────────────────

/// A version constraint on a required interface.
///
/// #505 deliberately ships **no semver solver**. This is a thin string wrapper
/// so the interface model can carry the author's intent (`">=1.2"`, `"~3.0"`,
/// `"1.x"`, …) without committing to a resolution algorithm. #506 (or a later
/// PR) may replace the inner representation with a parsed constraint; callers
/// should treat it as opaque for now.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VersionConstraint(pub String);

// ── Secret-specific value types ────────────────────────────────────────────

/// The blast-radius / lifetime a secret grant is scoped to.
///
/// This describes *which* secret a Capsule is asking for, never its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretScope {
    /// Scoped to a single running instance of the Capsule.
    CapsuleInstance,
    /// Shared across all instances of this Capsule (per install).
    Capsule,
    /// Belongs to the user and may be reused across Capsules with consent.
    User,
    /// Bound to the host device.
    Device,
}

/// How a granted secret should be *projected* into the running Capsule.
///
/// This is a reference / shape, not a value. The actual material is resolved
/// by a secret store at launch time (out of scope for #505).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "as")]
pub enum SecretProjection {
    /// Inject as an environment variable with the given name.
    Env { name: String },
    /// Mount at the given in-Capsule file path.
    File { path: String },
}

impl SecretProjection {
    /// The projection target (env var name or file path). Used by validation to
    /// reject empty projections — an empty target breaks injection at launch.
    pub fn target(&self) -> &str {
        match self {
            SecretProjection::Env { name } => name,
            SecretProjection::File { path } => path,
        }
    }
}

/// Coarse class of a hardware requirement. Intentionally small for #505.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareKind {
    Gpu,
    Cpu,
    Tpu,
    /// Any other accelerator / device class identified by `constraint`.
    Other,
}

// ── Provided interfaces ────────────────────────────────────────────────────

/// Something this Capsule exposes to the outside world.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "spec")]
pub enum ProvidedInterface {
    Service(ServiceProvide),
    Tool(ToolProvide),
    Runtime(RuntimeProvide),
    State(StateProvide),
    Ui(UiProvide),
    Api(ApiProvide),
}

/// A network/IPC service this Capsule serves (e.g. an HTTP API on a port).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServiceProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A tool / executable capability this Capsule offers to peers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A runtime this Capsule provides to peers (e.g. a language/runtime host).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RuntimeProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A named state surface this Capsule owns and can expose for binding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StateProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A user-facing UI surface this Capsule exposes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UiProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

/// A programmatic API surface this Capsule exposes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ApiProvide {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ProvidedInterface {
    /// The logical name used for duplicate detection and deterministic sorting.
    pub fn logical_name(&self) -> &str {
        match self {
            ProvidedInterface::Service(p) => &p.name,
            ProvidedInterface::Tool(p) => &p.name,
            ProvidedInterface::Runtime(p) => &p.name,
            ProvidedInterface::State(p) => &p.name,
            ProvidedInterface::Ui(p) => &p.name,
            ProvidedInterface::Api(p) => &p.name,
        }
    }

    /// Stable category discriminant (used for sort keys and dup detection).
    fn category(&self) -> &'static str {
        match self {
            ProvidedInterface::Service(_) => "service",
            ProvidedInterface::Tool(_) => "tool",
            ProvidedInterface::Runtime(_) => "runtime",
            ProvidedInterface::State(_) => "state",
            ProvidedInterface::Ui(_) => "ui",
            ProvidedInterface::Api(_) => "api",
        }
    }
}

// ── Required interfaces ────────────────────────────────────────────────────

/// Something this Capsule needs in order to launch and run.
///
/// Note the intentional asymmetry with [`ProvidedInterface`]: there are more
/// requirement categories than provide categories, precisely because most
/// requirements are satisfied by the host/platform/user rather than by a peer
/// Capsule. See the module docs and [`SatisfactionSource`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "spec")]
pub enum RequiredInterface {
    // ── peer-satisfiable: can be wired to another Capsule's `provides` ──
    Service(ServiceRequirement),
    Tool(ToolRequirement),
    Runtime(RuntimeRequirement),
    State(StateRequirement),
    // ── NOT peer-satisfiable: come from host / platform / user ──
    Secret(SecretRequirement),
    Network(NetworkRequirement),
    Capability(CapabilityRequirement),
    Hardware(HardwareRequirement),
    Port(PortRequirement),
    ProviderCapability(ProviderCapabilityRequirement),
}

/// Requires a service reachable by name/protocol/version.
///
/// Peer-satisfiable: a peer Capsule's [`ServiceProvide`] may satisfy this.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ServiceRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionConstraint>,
}

/// Requires a tool/executable capability. Peer-satisfiable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ToolRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionConstraint>,
}

/// Requires a runtime host. Peer-satisfiable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RuntimeRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionConstraint>,
}

/// Requires a state binding (a named, persistent state surface).
///
/// Peer-satisfiable *only* via a peer's [`StateProvide`]; otherwise satisfied
/// by a managed state binding ([`SatisfactionSource::StateBinding`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StateRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionConstraint>,
}

/// Requires a secret / auth credential.
///
/// This carries the *requirement* — name, scope, projection, optionality —
/// and **never** the secret value. There is intentionally no field that could
/// hold raw secret material; this is enforced by the type and asserted in
/// tests. Satisfied by a [`SatisfactionSource::UserSecretGrant`], never by a
/// peer Capsule's `provides`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SecretRequirement {
    pub name: String,
    pub scope: SecretScope,
    pub projection: SecretProjection,
    #[serde(default)]
    pub optional: bool,
}

/// Requires network access (e.g. egress to named hosts).
///
/// Not peer-satisfiable: network egress is a host/platform policy decision,
/// not something a peer Capsule can hand over. Satisfied by a managed resource.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetworkRequirement {
    pub logical_name: String,
    #[serde(default)]
    pub egress: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
}

/// Requires an OS/sandbox capability grant (e.g. raw sockets, ptrace).
///
/// Not peer-satisfiable: a capability is granted by the host capability policy
/// (a provider capability), never by wiring to a peer Capsule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    pub name: String,
}

/// Requires hardware (GPU/CPU/TPU/other).
///
/// Not peer-satisfiable: hardware is a managed host resource. `constraint` is
/// a free-form hint for #505 (e.g. `"vram>=8GB"`); no parser is shipped yet.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HardwareRequirement {
    pub kind: HardwareKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<String>,
}

/// Requires a port to be available (optionally a preferred number).
///
/// Not peer-satisfiable: port availability is decided by a port allocator /
/// remapper at launch, never by a peer Capsule's `provides`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortRequirement {
    pub logical_name: String,
    /// Preferred port. Already a `u16`, so it is always a valid port number;
    /// `None` means "any free port".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_port: Option<u16>,
    #[serde(default)]
    pub can_remap: bool,
}

/// Requires a capability of the *provider* (the platform/orchestrator) — e.g.
/// "can launch OCI containers", "supports GPU passthrough".
///
/// Not peer-satisfiable: this is a property of the executing provider, not of
/// a peer Capsule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProviderCapabilityRequirement {
    pub name: String,
}

impl RequiredInterface {
    /// The logical name used for duplicate detection and deterministic sorting.
    pub fn logical_name(&self) -> &str {
        match self {
            RequiredInterface::Service(r) => &r.name,
            RequiredInterface::Tool(r) => &r.name,
            RequiredInterface::Runtime(r) => &r.name,
            RequiredInterface::State(r) => &r.name,
            RequiredInterface::Secret(r) => &r.name,
            RequiredInterface::Network(r) => &r.logical_name,
            RequiredInterface::Capability(r) => &r.name,
            RequiredInterface::Hardware(_) => "", // keyed by kind below
            RequiredInterface::Port(r) => &r.logical_name,
            RequiredInterface::ProviderCapability(r) => &r.name,
        }
    }

    /// Stable category discriminant (used for sort keys and dup detection).
    fn category(&self) -> &'static str {
        match self {
            RequiredInterface::Service(_) => "service",
            RequiredInterface::Tool(_) => "tool",
            RequiredInterface::Runtime(_) => "runtime",
            RequiredInterface::State(_) => "state",
            RequiredInterface::Secret(_) => "secret",
            RequiredInterface::Network(_) => "network",
            RequiredInterface::Capability(_) => "capability",
            RequiredInterface::Hardware(_) => "hardware",
            RequiredInterface::Port(_) => "port",
            RequiredInterface::ProviderCapability(_) => "provider_capability",
        }
    }

    /// The duplicate-detection / sort key for this requirement. For hardware
    /// the logical name is empty, so we key on the kind instead.
    fn dedup_key(&self) -> String {
        match self {
            RequiredInterface::Hardware(r) => {
                format!("{}::{:?}", self.category(), r.kind)
            }
            other => format!("{}::{}", other.category(), other.logical_name()),
        }
    }

    /// Every source that *may* satisfy this requirement category.
    ///
    /// This is the single source of truth for the provides/requires boundary.
    /// A category is peer-wirable iff its list contains
    /// [`SatisfactionSource::ProvidedInterface`] — see [`Self::is_peer_satisfiable`],
    /// which is defined in terms of this method so the two can never disagree.
    ///
    /// Note `State` returns **two** sources: it can be wired to a peer's
    /// [`StateProvide`] *or* bound to a managed state surface. A caller (e.g.
    /// #506) that only ever looked at a single "the" source would wrongly route
    /// `State` to [`SatisfactionSource::StateBinding`] and miss the peer-wiring
    /// path — which is exactly why this returns a slice rather than one value.
    pub fn possible_satisfaction_sources(&self) -> &'static [SatisfactionSource] {
        use SatisfactionSource::*;
        match self {
            RequiredInterface::Service(_)
            | RequiredInterface::Tool(_)
            | RequiredInterface::Runtime(_) => &[ProvidedInterface],
            // Peer StateProvide OR a managed state binding may satisfy state.
            RequiredInterface::State(_) => &[ProvidedInterface, StateBinding],
            RequiredInterface::Secret(_) => &[UserSecretGrant],
            RequiredInterface::Network(_) | RequiredInterface::Hardware(_) => &[ManagedResource],
            RequiredInterface::Port(_) => &[PortAllocation],
            RequiredInterface::Capability(_) | RequiredInterface::ProviderCapability(_) => {
                &[ProviderCapability]
            }
        }
    }

    /// Whether a peer Capsule's [`ProvidedInterface`] could *ever* satisfy this
    /// requirement category. Defined in terms of
    /// [`Self::possible_satisfaction_sources`] so the boundary stays consistent.
    /// Secret/Network/Capability/Hardware/Port/ProviderCapability always return
    /// `false`.
    pub fn is_peer_satisfiable(&self) -> bool {
        self.possible_satisfaction_sources()
            .contains(&SatisfactionSource::ProvidedInterface)
    }
}

// ── The interface itself ───────────────────────────────────────────────────

/// The full provides/requires interface of a single Capsule.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CapsuleInterface {
    #[serde(default)]
    pub provides: Vec<ProvidedInterface>,
    #[serde(default)]
    pub requires: Vec<RequiredInterface>,
}

impl CapsuleInterface {
    /// Sort `provides` and `requires` into a deterministic, canonical order so
    /// that two interfaces built from the same set in different orders compare
    /// equal and serialize identically. Stable across runs (no hashing of
    /// pointers / addresses, only of the declared content).
    pub fn sort(&mut self) {
        self.provides.sort_by(|a, b| {
            (a.category(), a.logical_name()).cmp(&(b.category(), b.logical_name()))
        });
        self.requires
            .sort_by(|a, b| a.dedup_key().cmp(&b.dedup_key()));
    }

    /// Return a sorted clone. Convenience wrapper around [`Self::sort`].
    pub fn sorted(&self) -> Self {
        let mut c = self.clone();
        c.sort();
        c
    }

    /// Validate the interface, sorting it into canonical order on success.
    ///
    /// Validation rules (#505):
    /// - provided / required logical names must not be empty
    ///   (in particular required secret names and provided service names);
    /// - duplicate logical names within a category are rejected;
    /// - `preferred_port` is a `u16` so it is structurally always valid;
    /// - secret requirements never carry a value (enforced by the type — there
    ///   is no value field to populate).
    ///
    /// On success the interface is normalized via [`Self::sort`].
    pub fn validate(&mut self) -> Result<(), InterfaceError> {
        use std::collections::BTreeSet;

        // Provided names must be non-empty and unique within a category.
        let mut seen_provides: BTreeSet<String> = BTreeSet::new();
        for p in &self.provides {
            if p.logical_name().trim().is_empty() {
                return Err(InterfaceError::EmptyProvideName {
                    category: p.category(),
                });
            }
            let key = format!("{}::{}", p.category(), p.logical_name());
            if !seen_provides.insert(key) {
                return Err(InterfaceError::DuplicateProvide {
                    category: p.category(),
                    name: p.logical_name().to_string(),
                });
            }
        }

        // Required names must be non-empty (hardware is keyed by kind, so it is
        // exempt from the name check) and unique within a category.
        let mut seen_requires: BTreeSet<String> = BTreeSet::new();
        for r in &self.requires {
            let needs_name = !matches!(r, RequiredInterface::Hardware(_));
            if needs_name && r.logical_name().trim().is_empty() {
                return Err(InterfaceError::EmptyRequireName {
                    category: r.category(),
                });
            }
            // A secret's projection target (env var name / file path) must not
            // be empty — an empty target silently breaks injection at launch.
            if let RequiredInterface::Secret(s) = r {
                if s.projection.target().trim().is_empty() {
                    return Err(InterfaceError::EmptySecretProjection {
                        name: s.name.clone(),
                    });
                }
            }
            if !seen_requires.insert(r.dedup_key()) {
                return Err(InterfaceError::DuplicateRequire {
                    category: r.category(),
                    name: r.logical_name().to_string(),
                });
            }
        }

        self.sort();
        Ok(())
    }
}

// ── Validation errors ──────────────────────────────────────────────────────

/// Errors raised while validating a [`CapsuleInterface`] (#505).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InterfaceError {
    #[error("provided {category} interface has an empty name")]
    EmptyProvideName { category: &'static str },

    #[error("required {category} interface has an empty name")]
    EmptyRequireName { category: &'static str },

    #[error("duplicate provided {category} interface name '{name}'")]
    DuplicateProvide {
        category: &'static str,
        name: String,
    },

    #[error("duplicate required {category} interface name '{name}'")]
    DuplicateRequire {
        category: &'static str,
        name: String,
    },

    #[error("secret requirement '{name}' has an empty projection target")]
    EmptySecretProjection { name: String },
}

// ── Satisfaction predicate ─────────────────────────────────────────────────

/// Where a [`RequiredInterface`] can be satisfied *from*.
///
/// This enumerates the boundary that #506 composition must respect: only
/// [`SatisfactionSource::ProvidedInterface`] (and, for `State`, additionally
/// [`SatisfactionSource::StateBinding`]) involves a peer Capsule. The rest are
/// host / platform / user concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SatisfactionSource {
    /// Another Capsule's `provides`.
    ProvidedInterface,
    /// A secret granted by the user.
    UserSecretGrant,
    /// A capability of the executing provider / platform.
    ProviderCapability,
    /// A managed host resource (hardware, network egress, …).
    ManagedResource,
    /// A bound, persistent state surface.
    StateBinding,
    /// A port assigned by the allocator / remapper.
    PortAllocation,
}

/// The result of asking "could `provided` satisfy `required`?".
///
/// `compatible == false` with a non-`ProvidedInterface` `source` is how the
/// model expresses "this requirement is *never* satisfiable by a peer
/// Capsule — look at `source` instead".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SatisfactionCandidate {
    pub source: SatisfactionSource,
    pub compatible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SatisfactionCandidate {
    fn compatible() -> Self {
        SatisfactionCandidate {
            source: SatisfactionSource::ProvidedInterface,
            compatible: true,
            reason: None,
        }
    }

    fn incompatible_peer(reason: impl Into<String>) -> Self {
        SatisfactionCandidate {
            source: SatisfactionSource::ProvidedInterface,
            compatible: false,
            reason: Some(reason.into()),
        }
    }

    fn not_peer_satisfiable(source: SatisfactionSource, reason: impl Into<String>) -> Self {
        SatisfactionCandidate {
            source,
            compatible: false,
            reason: Some(reason.into()),
        }
    }
}

/// Pure predicate: can `provided` (a peer Capsule's `provides` entry) satisfy
/// `required`?
///
/// This is the **only** satisfaction logic in #505 and is intentionally
/// minimal — it does not touch a DB, a provider, or a secret store, and ships
/// no semver solver.
///
/// It only ever returns `compatible: true` for the peer-satisfiable categories
/// (Service / Runtime / Tool / State). For Secret / Hardware / Network / Port /
/// Capability / ProviderCapability it returns `compatible: false` along with
/// the [`SatisfactionSource`] those requirements *must* be satisfied from
/// instead — making the provides/requires boundary explicit for #506.
pub fn provided_interface_may_satisfy(
    required: &RequiredInterface,
    provided: &ProvidedInterface,
) -> SatisfactionCandidate {
    match required {
        RequiredInterface::Service(req) => match provided {
            ProvidedInterface::Service(prov) => service_match(req, prov),
            other => SatisfactionCandidate::incompatible_peer(format!(
                "service requirement '{}' cannot be satisfied by a {} provide",
                req.name,
                other.category()
            )),
        },
        RequiredInterface::Runtime(req) => match provided {
            ProvidedInterface::Runtime(prov) => named_match("runtime", &req.name, &prov.name),
            other => SatisfactionCandidate::incompatible_peer(format!(
                "runtime requirement '{}' cannot be satisfied by a {} provide",
                req.name,
                other.category()
            )),
        },
        RequiredInterface::Tool(req) => match provided {
            ProvidedInterface::Tool(prov) => named_match("tool", &req.name, &prov.name),
            other => SatisfactionCandidate::incompatible_peer(format!(
                "tool requirement '{}' cannot be satisfied by a {} provide",
                req.name,
                other.category()
            )),
        },
        RequiredInterface::State(req) => match provided {
            ProvidedInterface::State(prov) => named_match("state", &req.name, &prov.name),
            other => SatisfactionCandidate::incompatible_peer(format!(
                "state requirement '{}' cannot be satisfied by a {} provide",
                req.name,
                other.category()
            )),
        },

        // ── NOT satisfiable by any peer `provides` ──
        RequiredInterface::Secret(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::UserSecretGrant,
            "secret requirements are satisfied by a user secret grant, never by a peer Capsule's provides",
        ),
        RequiredInterface::Hardware(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::ManagedResource,
            "hardware (e.g. GPU) is a managed host resource, never satisfied by a peer Capsule's provides",
        ),
        RequiredInterface::Network(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::ManagedResource,
            "network egress is a host policy decision, never satisfied by a peer Capsule's provides",
        ),
        RequiredInterface::Port(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::PortAllocation,
            "port availability is decided by the port allocator, never satisfied by a peer Capsule's provides",
        ),
        RequiredInterface::Capability(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::ProviderCapability,
            "OS/sandbox capabilities are granted by host capability policy, never satisfied by a peer Capsule's provides",
        ),
        RequiredInterface::ProviderCapability(_) => SatisfactionCandidate::not_peer_satisfiable(
            SatisfactionSource::ProviderCapability,
            "provider capabilities are properties of the executing provider, never satisfied by a peer Capsule's provides",
        ),
    }
}

/// Match a service requirement against a service provide.
///
/// #505 matches on name and (when the requirement constrains it) protocol.
/// Version is carried but *not* used to decide compatibility — there is no
/// semver solver in this PR.
fn service_match(req: &ServiceRequirement, prov: &ServiceProvide) -> SatisfactionCandidate {
    if req.name != prov.name {
        return SatisfactionCandidate::incompatible_peer(format!(
            "service name mismatch: required '{}', provided '{}'",
            req.name, prov.name
        ));
    }
    if let Some(req_proto) = &req.protocol {
        match &prov.protocol {
            Some(prov_proto) if prov_proto == req_proto => {}
            Some(prov_proto) => {
                return SatisfactionCandidate::incompatible_peer(format!(
                    "service '{}' protocol mismatch: required '{}', provided '{}'",
                    req.name, req_proto, prov_proto
                ));
            }
            None => {
                return SatisfactionCandidate::incompatible_peer(format!(
                    "service '{}' requires protocol '{}' but provide declares none",
                    req.name, req_proto
                ));
            }
        }
    }
    SatisfactionCandidate::compatible()
}

/// Match by name only (runtime / tool / state).
fn named_match(kind: &str, req_name: &str, prov_name: &str) -> SatisfactionCandidate {
    if req_name == prov_name {
        SatisfactionCandidate::compatible()
    } else {
        SatisfactionCandidate::incompatible_peer(format!(
            "{kind} name mismatch: required '{req_name}', provided '{prov_name}'"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc_req(name: &str, proto: Option<&str>) -> RequiredInterface {
        RequiredInterface::Service(ServiceRequirement {
            name: name.to_string(),
            protocol: proto.map(String::from),
            version: None,
        })
    }

    fn svc_provide(name: &str, proto: Option<&str>) -> ProvidedInterface {
        ProvidedInterface::Service(ServiceProvide {
            name: name.to_string(),
            protocol: proto.map(String::from),
            version: None,
        })
    }

    fn secret_req(name: &str) -> RequiredInterface {
        RequiredInterface::Secret(SecretRequirement {
            name: name.to_string(),
            scope: SecretScope::CapsuleInstance,
            projection: SecretProjection::Env {
                name: name.to_string(),
            },
            optional: false,
        })
    }

    fn state_req(name: &str) -> RequiredInterface {
        RequiredInterface::State(StateRequirement {
            name: name.to_string(),
            version: None,
        })
    }

    fn state_provide(name: &str) -> ProvidedInterface {
        ProvidedInterface::State(StateProvide {
            name: name.to_string(),
            version: None,
        })
    }

    #[test]
    fn service_requirement_can_be_satisfied_by_matching_service_provide() {
        let req = svc_req("postgres", Some("tcp"));
        let prov = svc_provide("postgres", Some("tcp"));
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(candidate.compatible, "{:?}", candidate.reason);
        assert_eq!(candidate.source, SatisfactionSource::ProvidedInterface);
    }

    #[test]
    fn service_requirement_rejects_protocol_mismatch() {
        let req = svc_req("postgres", Some("tcp"));
        let prov = svc_provide("postgres", Some("http"));
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(!candidate.compatible);
        assert!(candidate.reason.unwrap().contains("protocol mismatch"));
    }

    #[test]
    fn service_requirement_rejects_name_mismatch() {
        let req = svc_req("postgres", None);
        let prov = svc_provide("redis", None);
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(!candidate.compatible);
    }

    #[test]
    fn state_requirement_can_be_satisfied_by_matching_state_provide() {
        let req = state_req("session");
        let prov = state_provide("session");
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(candidate.compatible, "{:?}", candidate.reason);
        assert_eq!(candidate.source, SatisfactionSource::ProvidedInterface);
        // A name mismatch is rejected.
        assert!(!provided_interface_may_satisfy(&req, &state_provide("other")).compatible);
        // A non-state provide cannot satisfy a state requirement.
        assert!(!provided_interface_may_satisfy(&req, &svc_provide("session", None)).compatible);
    }

    #[test]
    fn state_requirement_possible_sources_include_peer_and_state_binding() {
        let req = state_req("session");
        let sources = req.possible_satisfaction_sources();
        // State is BOTH peer-wirable (StateProvide) AND bindable to a managed
        // state surface — the boundary must expose both, never just one.
        assert!(sources.contains(&SatisfactionSource::ProvidedInterface));
        assert!(sources.contains(&SatisfactionSource::StateBinding));
        assert!(req.is_peer_satisfiable());

        // Cross-check: every category's peer-satisfiability agrees with whether
        // ProvidedInterface is among its possible sources (no drift between the
        // two methods).
        for r in [
            svc_req("s", None),
            RequiredInterface::Tool(ToolRequirement {
                name: "t".into(),
                version: None,
            }),
            RequiredInterface::Runtime(RuntimeRequirement {
                name: "rt".into(),
                version: None,
            }),
            state_req("st"),
            secret_req("SECRET"),
            RequiredInterface::Network(NetworkRequirement {
                logical_name: "n".into(),
                egress: true,
                hosts: vec![],
            }),
            RequiredInterface::Port(PortRequirement {
                logical_name: "p".into(),
                preferred_port: None,
                can_remap: false,
            }),
            RequiredInterface::Hardware(HardwareRequirement {
                kind: HardwareKind::Gpu,
                constraint: None,
            }),
            RequiredInterface::Capability(CapabilityRequirement { name: "c".into() }),
            RequiredInterface::ProviderCapability(ProviderCapabilityRequirement {
                name: "pc".into(),
            }),
        ] {
            assert_eq!(
                r.is_peer_satisfiable(),
                r.possible_satisfaction_sources()
                    .contains(&SatisfactionSource::ProvidedInterface),
                "boundary drift for {:?}",
                r.category()
            );
        }
    }

    #[test]
    fn secret_requirement_rejects_empty_projection_target() {
        let mut iface = CapsuleInterface {
            provides: vec![],
            requires: vec![RequiredInterface::Secret(SecretRequirement {
                name: "TOKEN".into(),
                scope: SecretScope::CapsuleInstance,
                projection: SecretProjection::Env { name: "".into() },
                optional: false,
            })],
        };
        let err = iface.validate().unwrap_err();
        assert!(matches!(err, InterfaceError::EmptySecretProjection { .. }));
    }

    #[test]
    fn secret_requirement_is_not_satisfied_by_peer_provides() {
        let req = secret_req("OPENAI_API_KEY");
        // Try every kind of provide — none may satisfy a secret.
        let provides = [
            svc_provide("OPENAI_API_KEY", Some("tcp")),
            ProvidedInterface::Tool(ToolProvide {
                name: "OPENAI_API_KEY".to_string(),
                version: None,
            }),
            ProvidedInterface::Runtime(RuntimeProvide {
                name: "OPENAI_API_KEY".to_string(),
                version: None,
            }),
        ];
        for prov in &provides {
            let candidate = provided_interface_may_satisfy(&req, prov);
            assert!(!candidate.compatible);
            assert_eq!(candidate.source, SatisfactionSource::UserSecretGrant);
        }
        assert!(!req.is_peer_satisfiable());
    }

    #[test]
    fn hardware_requirement_is_not_satisfied_by_peer_provides() {
        let req = RequiredInterface::Hardware(HardwareRequirement {
            kind: HardwareKind::Gpu,
            constraint: Some("vram>=8GB".to_string()),
        });
        let prov = svc_provide("gpu", None);
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(!candidate.compatible);
        assert_eq!(candidate.source, SatisfactionSource::ManagedResource);
        assert!(!req.is_peer_satisfiable());
    }

    #[test]
    fn port_requirement_requires_allocation_not_peer_provide() {
        let req = RequiredInterface::Port(PortRequirement {
            logical_name: "http".to_string(),
            preferred_port: Some(8080),
            can_remap: true,
        });
        let prov = svc_provide("http", Some("tcp"));
        let candidate = provided_interface_may_satisfy(&req, &prov);
        assert!(!candidate.compatible);
        assert_eq!(candidate.source, SatisfactionSource::PortAllocation);
        assert!(!req.is_peer_satisfiable());
    }

    #[test]
    fn network_and_provider_capability_are_not_peer_satisfiable() {
        let net = RequiredInterface::Network(NetworkRequirement {
            logical_name: "egress".to_string(),
            egress: true,
            hosts: vec!["api.openai.com".to_string()],
        });
        let pcap = RequiredInterface::ProviderCapability(ProviderCapabilityRequirement {
            name: "oci-launch".to_string(),
        });
        let cap = RequiredInterface::Capability(CapabilityRequirement {
            name: "net_admin".to_string(),
        });
        let prov = svc_provide("anything", None);
        assert!(!provided_interface_may_satisfy(&net, &prov).compatible);
        assert!(!provided_interface_may_satisfy(&pcap, &prov).compatible);
        assert!(!provided_interface_may_satisfy(&cap, &prov).compatible);
        assert!(!net.is_peer_satisfiable());
        assert!(!pcap.is_peer_satisfiable());
        assert!(!cap.is_peer_satisfiable());
        assert_eq!(
            provided_interface_may_satisfy(&pcap, &prov).source,
            SatisfactionSource::ProviderCapability
        );
    }

    #[test]
    fn required_secret_rejects_empty_name() {
        let mut iface = CapsuleInterface {
            provides: vec![],
            requires: vec![secret_req("")],
        };
        let err = iface.validate().unwrap_err();
        assert!(matches!(err, InterfaceError::EmptyRequireName { .. }));
    }

    #[test]
    fn provided_service_rejects_empty_name() {
        let mut iface = CapsuleInterface {
            provides: vec![svc_provide("", None)],
            requires: vec![],
        };
        let err = iface.validate().unwrap_err();
        assert!(matches!(err, InterfaceError::EmptyProvideName { .. }));
    }

    #[test]
    fn duplicate_provide_names_are_rejected() {
        let mut iface = CapsuleInterface {
            provides: vec![svc_provide("api", None), svc_provide("api", Some("tcp"))],
            requires: vec![],
        };
        let err = iface.validate().unwrap_err();
        assert!(matches!(err, InterfaceError::DuplicateProvide { .. }));
    }

    #[test]
    fn duplicate_require_names_are_rejected() {
        let mut iface = CapsuleInterface {
            provides: vec![],
            requires: vec![svc_req("db", None), svc_req("db", Some("tcp"))],
        };
        let err = iface.validate().unwrap_err();
        assert!(matches!(err, InterfaceError::DuplicateRequire { .. }));
    }

    #[test]
    fn secret_interface_never_contains_secret_value() {
        // The secret value must never be representable in the interface model.
        // The type has no value field; assert the serialized form carries only
        // the requirement shape (name / scope / projection / optional) and no
        // field that could hold raw secret material.
        let iface = CapsuleInterface {
            provides: vec![],
            requires: vec![secret_req("OPENAI_API_KEY")],
        };
        let json = serde_json::to_string(&iface).unwrap();
        assert!(json.contains("OPENAI_API_KEY"));
        assert!(json.contains("capsule-instance"));
        // No raw-value fields of any spelling.
        for forbidden in ["\"value\"", "\"secret\":", "\"plaintext\"", "\"material\""] {
            assert!(
                !json.contains(forbidden),
                "secret interface leaked a value field {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn interface_serialization_roundtrip() {
        let mut iface = CapsuleInterface {
            provides: vec![
                svc_provide("api", Some("http")),
                ProvidedInterface::Ui(UiProvide {
                    name: "dashboard".to_string(),
                    route: Some("/".to_string()),
                }),
            ],
            requires: vec![
                svc_req("postgres", Some("tcp")),
                secret_req("OPENAI_API_KEY"),
                RequiredInterface::Port(PortRequirement {
                    logical_name: "http".to_string(),
                    preferred_port: Some(8080),
                    can_remap: true,
                }),
                RequiredInterface::Hardware(HardwareRequirement {
                    kind: HardwareKind::Gpu,
                    constraint: None,
                }),
            ],
        };
        iface.validate().unwrap();

        let json = serde_json::to_string_pretty(&iface).unwrap();
        let back: CapsuleInterface = serde_json::from_str(&json).unwrap();
        assert_eq!(iface, back);
    }

    #[test]
    fn interface_sorting_is_deterministic() {
        let a = CapsuleInterface {
            provides: vec![svc_provide("zeta", None), svc_provide("alpha", None)],
            requires: vec![
                RequiredInterface::Port(PortRequirement {
                    logical_name: "http".to_string(),
                    preferred_port: None,
                    can_remap: false,
                }),
                secret_req("TOKEN"),
                svc_req("db", None),
            ],
        };
        // Same content, different insertion order.
        let b = CapsuleInterface {
            provides: vec![svc_provide("alpha", None), svc_provide("zeta", None)],
            requires: vec![
                svc_req("db", None),
                RequiredInterface::Port(PortRequirement {
                    logical_name: "http".to_string(),
                    preferred_port: None,
                    can_remap: false,
                }),
                secret_req("TOKEN"),
            ],
        };
        assert_eq!(a.sorted(), b.sorted());
        // Idempotent.
        assert_eq!(a.sorted(), a.sorted().sorted());
    }
}
