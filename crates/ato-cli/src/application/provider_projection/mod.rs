//! Provider projection boundary (#501).
//!
//! Ato treats a provider (OCI/podman, web, wasm, managed cloud, …) as **one
//! projection** of a resolved Capsule — never as the product abstraction and
//! never as a raw `docker`/`podman` command generator. The source of truth is:
//!
//! ```text
//! Resolved Capsule / Launch Conditions
//!   -> Provider Projection Plan       (this module)
//!   -> Provider-specific invocation   (e.g. `podman create` argv)
//!   -> Provider evidence              (container id, pid, logs — NOT identity)
//! ```
//!
//! not:
//!
//! ```text
//! capsule.toml -> docker/podman command string -> container started
//! ```
//!
//! ## Identity boundaries this module enforces
//!
//! ```text
//! image digest        != capsule identity   (it is image/provider evidence)
//! container id        != execution identity
//! provider process id != execution identity
//! session id/log path != resolved capsule identity
//! command/argv string != source of truth    (it is derived from the plan)
//! ```
//!
//! An OCI image digest may be a graph node or a plan field, but it must not
//! become the *whole* Capsule identity. A tag-only image whose digest is not
//! resolvable is represented as partial/unpinned provider evidence rather than
//! silently treated as fully pinned (see [`oci::OciImageDigest`]).
//!
//! ## Scope of this slice
//!
//! This is the first concrete #501 slice and is intentionally narrow: it
//! introduces the boundary type and routes the existing OCI launch-command
//! construction through an [`oci::OciProjectionPlan`]. It does **not** populate
//! full NodeReceipt/EdgeReceipt evidence (#493), implement cross-device
//! placement (#509), or change install admission (#508).

#![allow(dead_code)]

pub(crate) mod oci;

use serde::Serialize;

/// The class of realization a provider performs for a resolved Capsule.
///
/// `kind` is coarse and stable; the concrete realizer (podman vs docker vs
/// youki, …) lives in [`ProviderId::name`]. Many names share one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProviderKind {
    SourceNative,
    Oci,
    Web,
    Wasm,
    ManagedCloud,
    ExternalRunner,
}

/// Identifies the provider that projects a Capsule: a coarse [`ProviderKind`]
/// plus the concrete realizer name (`podman`, `docker`, `youki`, `containerd`,
/// `ato-cloud`, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct ProviderId {
    pub kind: ProviderKind,
    pub name: String,
}

impl ProviderId {
    pub(crate) fn new(kind: ProviderKind, name: impl Into<String>) -> Self {
        Self {
            kind,
            name: name.into(),
        }
    }
}

/// Capabilities a provider must offer to realize a given projection plan.
///
/// A plan records the capabilities its launch conditions *require*; a future
/// placement step (#509) can compare these against a provider's *offered*
/// capabilities without ever inspecting a raw `docker`/`podman` command string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub(crate) struct ProviderCapabilities {
    pub filesystem_projection: bool,
    pub read_only_mounts: bool,
    pub persistent_state: bool,
    pub network_policy: bool,
    pub ingress_routing: bool,
    pub env_projection: bool,
    pub secret_projection: bool,
    pub uid_gid_control: bool,
    pub readiness_probe: bool,
    pub service_network_alias: bool,
}

/// Provider-side *evidence* produced when a plan is realized.
///
/// These are session-local runtime facts and are explicitly **not** part of the
/// projection identity: a container id is the provider's handle, not the
/// execution identity; a pid is a live process, not identity; a log path is
/// where output landed, not the resolved Capsule.
///
/// This PR only models the boundary — it does not populate full
/// NodeReceipt/EdgeReceipt evidence. That remains tracked by #493.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderProjectionEvidence {
    /// Runtime container id (e.g. stdout of `podman create`). NOT execution
    /// identity.
    pub container_id: Option<String>,
    /// Provider process id. NOT execution identity.
    pub provider_pid: Option<u32>,
    /// Session log path. NOT resolved-Capsule identity.
    pub log_path: Option<String>,
}
