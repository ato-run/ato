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

pub(crate) mod oci;
pub(crate) mod strict_oci;

use serde::Serialize;

/// The class of realization a provider performs for a resolved Capsule.
///
/// `kind` is coarse and stable; the concrete realizer (podman vs docker vs
/// youki, …) lives in [`ProviderId::name`]. Many names share one kind.
///
/// Only `Oci` is constructed in this first slice; the remaining variants are
/// the forward-looking provider taxonomy from #501 (consumed by placement,
/// #509). The `allow` should be dropped once a second provider lands.
#[allow(dead_code)]
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

impl ProviderKind {
    /// Stable kebab label, matching the serde representation.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SourceNative => "source-native",
            Self::Oci => "oci",
            Self::Web => "web",
            Self::Wasm => "wasm",
            Self::ManagedCloud => "managed-cloud",
            Self::ExternalRunner => "external-runner",
        }
    }
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

    /// Coarse provider version/family label, e.g. `"oci-podman-v1"`. Value-free
    /// (no host path or machine handle); suitable for receipt evidence (#501).
    pub(crate) fn family(&self) -> String {
        format!("{}-{}-v1", self.kind.label(), self.name)
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

impl ProviderCapabilities {
    /// Kebab labels of the enabled capabilities, sorted. Suitable for a
    /// receipt-safe summary (flags only, no launch values).
    pub(crate) fn enabled_labels(&self) -> Vec<String> {
        let mut labels = Vec::new();
        let entries = [
            (self.filesystem_projection, "filesystem-projection"),
            (self.read_only_mounts, "read-only-mounts"),
            (self.persistent_state, "persistent-state"),
            (self.network_policy, "network-policy"),
            (self.ingress_routing, "ingress-routing"),
            (self.env_projection, "env-projection"),
            (self.secret_projection, "secret-projection"),
            (self.uid_gid_control, "uid-gid-control"),
            (self.readiness_probe, "readiness-probe"),
            (self.service_network_alias, "service-network-alias"),
        ];
        for (enabled, label) in entries {
            if enabled {
                labels.push(label.to_string());
            }
        }
        labels.sort();
        labels
    }
}

/// Provider-side *evidence* produced when a plan is realized.
///
/// These are session-local runtime facts and are explicitly **not** part of the
/// projection identity: a container id is the provider's handle, not the
/// execution identity; a pid is a live process, not identity; a log path is
/// where output landed, not the resolved Capsule.
///
/// This PR only models the boundary — it does not populate full
/// NodeReceipt/EdgeReceipt evidence. That remains tracked by #493, which will
/// be the first consumer of this shape; defined now so the boundary is explicit.
#[allow(dead_code)]
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
