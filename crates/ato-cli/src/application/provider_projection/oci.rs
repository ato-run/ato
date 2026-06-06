//! OCI provider projection plan (#501).
//!
//! [`OciProjectionPlan`] is the **source of truth** for an OCI launch: the
//! `docker`/`podman` invocation is *rendered from* the plan (see
//! [`OciProjectionPlan::render_podman_create_argv`]), not the other way around.
//! The plan is built from the resolved launch conditions of a single container
//! ([`OciContainerRequest`]) and carries no runtime evidence (container id, pid,
//! log path) — those live in [`super::ProviderProjectionEvidence`].

use std::collections::BTreeMap;

use capsule_core::execution_identity::{
    OciEnforcementStatus, OciImageDigestStatus, OciMountReceiptEvidence, OciPortReceiptEvidence,
    OciProviderReceiptEvidence,
};
use capsule_core::realization::RedactedProjectionCommand;
use capsule_core::runtime::oci::{
    OciContainerRequest, OciMountSourceKind, OciMountSpec, OciPortSpec,
};
use capsule_core::types::OciPlatform;

use super::{ProviderCapabilities, ProviderId, ProviderKind};

/// How well the launch image is pinned.
///
/// An image *digest* is provider/image **evidence**, not the Capsule identity
/// (#501). A tag-only reference is represented as [`OciImageDigest::Unpinned`]
/// rather than silently treated as pinned, so downstream evidence can record
/// "digest not resolved at plan time" honestly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OciImageDigest {
    /// `repo@sha256:<64 hex>` — a fully pinned digest. This is image evidence,
    /// not the Capsule identity.
    Pinned(String),
    /// Tag-only (or otherwise undigested) reference; the realized digest is not
    /// known at plan time. Partial provider evidence — not "fully resolved".
    Unpinned,
}

impl OciImageDigest {
    // Boundary accessor exercised by tests; the first non-test consumer is the
    // provider-evidence work in #493.
    #[allow(dead_code)]
    pub(crate) fn is_pinned(&self) -> bool {
        matches!(self, Self::Pinned(_))
    }

    /// Stable label for canonical/identity rendering. Deliberately reveals the
    /// *state* (pinned vs unpinned) without elevating the digest to identity.
    /// Only reached via [`OciProjectionPlan::canonical_identity`] (itself
    /// test-only for now), so allow until that is wired in.
    #[allow(dead_code)]
    fn label(&self) -> String {
        match self {
            Self::Pinned(digest) => format!("pinned({digest})"),
            Self::Unpinned => "unpinned".to_string(),
        }
    }
}

/// The image a plan launches, plus its digest evidence.
///
/// `reference` is a launch condition (changing it changes the plan); `digest`
/// is evidence about how well that reference is pinned. Neither is the whole
/// Capsule identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciImageRef {
    /// The reference as launched (`repo:tag` or `repo@sha256:…`).
    pub reference: String,
    /// Digest evidence. NOT the Capsule identity.
    pub digest: OciImageDigest,
}

impl OciImageRef {
    pub(crate) fn parse(reference: &str) -> Self {
        let digest = parse_pinned_digest(reference)
            .map(OciImageDigest::Pinned)
            .unwrap_or(OciImageDigest::Unpinned);
        Self {
            reference: reference.to_string(),
            digest,
        }
    }
}

/// Extract a `sha256:<64 hex>` digest from a `repo@sha256:…` reference, if and
/// only if it is well-formed. Returns `None` for tag-only references so the
/// caller models them as [`OciImageDigest::Unpinned`].
fn parse_pinned_digest(reference: &str) -> Option<String> {
    let at = reference.find("@sha256:")?;
    let digest = &reference[at + 1..];
    let hex = digest.strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(digest.to_string())
    } else {
        None
    }
}

/// One mount in a projection plan, in stable (rendering-ready, evidence-free)
/// form. Built from [`OciMountSpec`]; carries exactly what is needed both to
/// render the runtime invocation and to compare launch conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciMountProjection {
    /// Host path (bind) or engine-managed volume name, per `engine_volume`.
    pub source: String,
    pub target: String,
    pub readonly: bool,
    /// `true` when `source` names an engine-managed volume rather than a host
    /// bind path (#444).
    pub engine_volume: bool,
    /// `true` when the engine should initialize ownership for the container
    /// user (Podman `:U`, #428).
    pub ownership_init: bool,
    /// For engine volumes: whether cleanup should delete the volume on stop
    /// (ephemeral) vs. keep it (persistent state).
    pub remove_on_stop: bool,
}

impl OciMountProjection {
    pub(crate) fn from_spec(spec: &OciMountSpec) -> Self {
        let (engine_volume, remove_on_stop) = match spec.source_kind {
            OciMountSourceKind::EngineVolume { remove_on_stop } => (true, remove_on_stop),
            OciMountSourceKind::BindPath => (false, false),
        };
        Self {
            source: spec.source.clone(),
            target: spec.target.clone(),
            readonly: spec.readonly,
            engine_volume,
            ownership_init: spec.ownership.is_some(),
            remove_on_stop,
        }
    }

    /// A *state binding* is durable, engine-managed state that must survive
    /// across restarts: an engine-managed volume that is not removed on stop.
    pub(crate) fn is_persistent_state_binding(&self) -> bool {
        self.engine_volume && !self.remove_on_stop
    }
}

/// One published port in a projection plan, in stable form. Built from
/// [`OciPortSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciPortProjection {
    pub container_port: u16,
    pub host_port: Option<u16>,
    pub protocol: String,
    pub host_ip: Option<String>,
}

impl OciPortProjection {
    pub(crate) fn from_spec(spec: &OciPortSpec) -> Self {
        Self {
            container_port: spec.container_port,
            host_port: spec.host_port,
            protocol: spec.protocol.clone(),
            host_ip: spec.host_ip.clone(),
        }
    }
}

/// Network projection: the service-internal network the container joins, plus
/// the service aliases it answers to and any extra `/etc/hosts` entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OciNetworkProjection {
    /// Internal service network name (`--network`), if any.
    pub network: Option<String>,
    /// Service network aliases (`--network-alias`): the in-graph service edges
    /// other services dial by name.
    pub service_aliases: Vec<String>,
    /// Extra `/etc/hosts` entries (`--add-host`), e.g.
    /// `host.containers.internal:host-gateway`.
    pub extra_hosts: Vec<String>,
}

/// The OCI projection plan: the source of truth for one container's launch.
///
/// `docker`/`podman` argv is **rendered from** this plan, never the reverse.
/// The plan holds only resolved launch conditions and no runtime evidence —
/// there is deliberately no field for container id, pid, or log path.
///
/// The requested [`OciProjectionPlan::name`] is a *rendering input* (the
/// `--name` handle, which may embed a session id); it is excluded from
/// [`OciProjectionPlan::canonical_identity`] precisely because session-local
/// naming is not part of the resolved-Capsule launch identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciProjectionPlan {
    pub provider_id: ProviderId,
    pub image: OciImageRef,
    /// Optional platform override for emulated execution.
    pub platform: Option<OciPlatform>,
    /// Requested container name (`--name`). A rendering input, NOT identity.
    pub name: String,
    /// Command/entrypoint override (`podman create … <image> <argv…>`). Empty
    /// keeps the image's baked-in entrypoint.
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub user: Option<String>,
    /// Environment projection, canonicalized (sorted) so the plan is the
    /// deterministic source of truth rather than HashMap iteration order.
    pub env_projection: BTreeMap<String, String>,
    /// Labels that are *launch conditions* — part of the resolved-Capsule
    /// identity (e.g. `io.ato.target`, user-declared labels). Canonicalized
    /// (sorted). Rendered to `--label` **and** included in
    /// [`OciProjectionPlan::canonical_identity`].
    pub launch_labels: BTreeMap<String, String>,
    /// Provider bookkeeping / session-local labels (e.g. `io.ato.session_id`,
    /// `io.ato.execution_id`, `io.ato.managed`, `io.ato.provider`). These are
    /// how the provider tags and later reaps its own containers — **not** part
    /// of the Capsule identity. Rendered to `--label` for behavior parity, but
    /// deliberately excluded from [`OciProjectionPlan::canonical_identity`] so a
    /// session id can never leak into identity (#501).
    pub provider_metadata_labels: BTreeMap<String, String>,
    /// Mounts, including state bindings (see
    /// [`OciMountProjection::is_persistent_state_binding`]).
    pub mounts: Vec<OciMountProjection>,
    pub ports: Vec<OciPortProjection>,
    pub network: OciNetworkProjection,
    /// Capabilities these launch conditions require of a realizing provider.
    pub capabilities_required: ProviderCapabilities,
}

impl OciProjectionPlan {
    /// Project a resolved single-container launch ([`OciContainerRequest`]) into
    /// the plan. This is the only place launch inputs become a plan; the runtime
    /// invocation is then rendered from the plan.
    pub(crate) fn from_container_request(request: &OciContainerRequest) -> Self {
        let mounts: Vec<OciMountProjection> = request
            .mounts
            .iter()
            .map(OciMountProjection::from_spec)
            .collect();
        let capabilities_required = required_capabilities(request, &mounts);
        Self {
            provider_id: ProviderId::new(ProviderKind::Oci, "podman"),
            image: OciImageRef::parse(&request.image),
            platform: request.platform.clone(),
            name: request.name.clone(),
            argv: request.cmd.clone(),
            cwd: request.working_dir.clone(),
            user: request.user.clone(),
            env_projection: request
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            launch_labels: request
                .labels
                .iter()
                .filter(|(k, _)| !is_provider_metadata_label(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            provider_metadata_labels: request
                .labels
                .iter()
                .filter(|(k, _)| is_provider_metadata_label(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            mounts,
            ports: request
                .ports
                .iter()
                .map(OciPortProjection::from_spec)
                .collect(),
            network: OciNetworkProjection {
                network: request.network.clone(),
                service_aliases: request.aliases.clone(),
                extra_hosts: request.extra_hosts.clone(),
            },
            capabilities_required,
        }
    }

    /// Project this plan into receipt-safe OCI provider evidence (#493).
    ///
    /// This is the bridge from the #516 provider projection boundary to a
    /// persisted receipt. It carries only receipt-safe facts:
    ///
    /// * env variable **names** (`env_projection` keys) — never values;
    /// * the image reference + a pinned/unpinned digest *status*;
    /// * mount *targets* and flags — never source host paths;
    /// * declared container ports, network aliases, and required capability
    ///   flags.
    ///
    /// Deliberately excluded: resolved env values, argv, the requested
    /// container name, and `provider_metadata_labels` (session id, managed
    /// flag, …) — all session-local provider data that must not become
    /// execution identity.
    // Enforcement-unaware convenience used by tests; the launch/receipt paths use
    // `receipt_evidence_with` so they always record the provider's enforcement
    // status. Kept as the explicit `Unknown`-enforcement boundary.
    #[allow(dead_code)]
    pub(crate) fn receipt_evidence(&self) -> OciProviderReceiptEvidence {
        // Without provider enforcement facts the status is honestly `Unknown`
        // (treated as not-enforced by strict mode). The enforcement-aware path
        // is `receipt_evidence_with`.
        self.receipt_evidence_with(OciEnforcementStatus::Unknown, OciEnforcementStatus::Unknown)
    }

    /// As [`Self::receipt_evidence`], but records the selected provider's typed
    /// enforcement status for the declared network and capability policy (#501).
    pub(crate) fn receipt_evidence_with(
        &self,
        network_enforcement: OciEnforcementStatus,
        capability_enforcement: OciEnforcementStatus,
    ) -> OciProviderReceiptEvidence {
        OciProviderReceiptEvidence {
            provider_kind: self.provider_id.kind.label().to_string(),
            provider_name: self.provider_id.name.clone(),
            image_reference: self.image.reference.clone(),
            image_digest_status: match &self.image.digest {
                OciImageDigest::Pinned(digest) => OciImageDigestStatus::Pinned {
                    digest: digest.clone(),
                },
                OciImageDigest::Unpinned => OciImageDigestStatus::Unpinned,
            },
            platform: self
                .platform
                .as_ref()
                .map(|p| format!("{}/{}", p.os, p.architecture)),
            // BTreeMap keys are already sorted; values are never read.
            env_keys: self.env_projection.keys().cloned().collect(),
            mounts: self
                .mounts
                .iter()
                .map(|m| OciMountReceiptEvidence {
                    target: m.target.clone(),
                    readonly: m.readonly,
                    engine_volume: m.engine_volume,
                    persistent_state: m.is_persistent_state_binding(),
                })
                .collect(),
            ports: self
                .ports
                .iter()
                .map(|p| OciPortReceiptEvidence {
                    container_port: p.container_port,
                    protocol: p.protocol.clone(),
                })
                .collect(),
            network_aliases: self.service_edges().to_vec(),
            capabilities_required: self.capabilities_required.enabled_labels(),
            provider_version: Some(self.provider_id.family()),
            network_enforcement_status: network_enforcement,
            capability_enforcement_status: capability_enforcement,
            // Derived projection evidence: the rendered argv reduced to a value-
            // free shape (flags survive, every value becomes `<redacted>`). Never
            // identity, never a raw command.
            //
            // Note: no hashed identity *summary* is recorded here. The plan's
            // `canonical_identity` embeds env values and mount source paths, and
            // even a digest of those is a correlation/guessing oracle — secrets,
            // env values, and host paths must never enter receipt evidence in any
            // form (#501). A receipt-safe projection fingerprint can be added in a
            // later slice, computed strictly from the redaction-safe fields below.
            derived_command_redacted: self.redacted_derived_command(),
            // Set per-service by the orchestration path; `None` for single-target.
            service_label: None,
        }
    }

    /// The derived `podman create` argv reduced to a redacted, value-free shape.
    /// Rendered with a neutral host (so `--platform` emulation does not depend on
    /// the running machine) and redacted via [`RedactedProjectionCommand`]. A
    /// render error (e.g. a read-only-ownership conflict) yields an empty shape —
    /// the evidence is best-effort and never blocks on its own.
    pub(crate) fn redacted_derived_command(&self) -> Vec<String> {
        let host = self.platform.clone().unwrap_or_else(|| OciPlatform {
            os: "linux".to_string(),
            architecture: "amd64".to_string(),
            variant: None,
        });
        match self.render_podman_create_argv(&host) {
            Ok(argv) => RedactedProjectionCommand::from_argv("podman-create", &argv).argv_shape,
            Err(_) => Vec::new(),
        }
    }

    /// Durable, engine-managed state bindings in this plan.
    // Boundary accessor exercised by tests; consumed by placement (#509).
    #[allow(dead_code)]
    pub(crate) fn state_bindings(&self) -> impl Iterator<Item = &OciMountProjection> {
        self.mounts
            .iter()
            .filter(|m| m.is_persistent_state_binding())
    }

    /// The in-graph service edges (network aliases) this container answers to.
    pub(crate) fn service_edges(&self) -> &[String] {
        &self.network.service_aliases
    }

    /// A deterministic, session-independent rendering of the launch *identity*.
    ///
    /// This excludes session-local rendering inputs — the requested container
    /// `name` and the [`OciProjectionPlan::provider_metadata_labels`] (e.g.
    /// `io.ato.session_id`) — and contains no runtime evidence (container id,
    /// pid, log path): those fields do not exist on the plan. Only
    /// [`OciProjectionPlan::launch_labels`] (launch conditions) participate. The
    /// image digest appears only as labeled evidence (`pinned`/`unpinned`),
    /// never as a standalone identity.
    ///
    /// Until a canonical hashing scheme for projection plans exists, this is a
    /// stable serialization used for equality/snapshot tests (#501); it is not
    /// a permanent hash scheme.
    // The projection-identity primitive: exercised by tests now, consumed by the
    // realization contract / receipts (#498, #493) next. Allow until wired in.
    #[allow(dead_code)]
    pub(crate) fn canonical_identity(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "provider={:?}:{}\n",
            self.provider_id.kind, self.provider_id.name
        ));
        out.push_str(&format!("image.reference={}\n", self.image.reference));
        out.push_str(&format!("image.digest={}\n", self.image.digest.label()));
        out.push_str(&format!("platform={:?}\n", self.platform));
        out.push_str(&format!("argv={:?}\n", self.argv));
        out.push_str(&format!("cwd={:?}\n", self.cwd));
        out.push_str(&format!("user={:?}\n", self.user));
        for (k, v) in &self.env_projection {
            out.push_str(&format!("env[{k}]={v}\n"));
        }
        // Only launch labels are identity-bearing; provider_metadata_labels
        // (session id, managed flag, …) are deliberately excluded.
        for (k, v) in &self.launch_labels {
            out.push_str(&format!("label[{k}]={v}\n"));
        }
        for m in &self.mounts {
            out.push_str(&format!(
                "mount={}->{} ro={} engine_volume={} ownership_init={} remove_on_stop={}\n",
                m.source, m.target, m.readonly, m.engine_volume, m.ownership_init, m.remove_on_stop
            ));
        }
        for p in &self.ports {
            out.push_str(&format!(
                "port={}:{:?}/{} ip={:?}\n",
                p.container_port, p.host_port, p.protocol, p.host_ip
            ));
        }
        out.push_str(&format!("network={:?}\n", self.network.network));
        out.push_str(&format!("aliases={:?}\n", self.network.service_aliases));
        out.push_str(&format!("extra_hosts={:?}\n", self.network.extra_hosts));
        out.push_str(&format!("caps={:?}\n", self.capabilities_required));
        out
    }
}

/// True when a label key is provider bookkeeping / session-local rather than a
/// launch condition.
///
/// These are the labels the OCI executors stamp onto every container so the
/// provider can find and reap *its own* containers — they encode the session,
/// not the resolved Capsule. They must never enter the projection identity (a
/// session id is not Capsule identity, #501). `io.ato.target` is intentionally
/// **not** here: it names which target/service this container realizes, which
/// is a genuine launch condition.
///
/// Keep this in sync with the labels stamped by the OCI executors
/// (`adapters/runtime/executors/oci_*`); any new provider-bookkeeping label
/// must be added here so it stays out of identity.
fn is_provider_metadata_label(key: &str) -> bool {
    matches!(
        key,
        "io.ato.session_id"
            | "io.ato.session"
            | "io.ato.execution_id"
            | "io.ato.managed"
            | "io.ato.provider"
    )
}

/// Derive the capabilities a launch requires from its resolved conditions.
/// Only facts present in the launch inputs are reported; fields that are not
/// determinable at the single-container request layer (ingress routing,
/// distinct secret projection, readiness probes — all handled upstream by the
/// orchestrator) stay `false` rather than being guessed.
fn required_capabilities(
    request: &OciContainerRequest,
    mounts: &[OciMountProjection],
) -> ProviderCapabilities {
    ProviderCapabilities {
        filesystem_projection: !mounts.is_empty(),
        read_only_mounts: mounts.iter().any(|m| m.readonly),
        persistent_state: mounts.iter().any(|m| m.is_persistent_state_binding()),
        network_policy: request.network.is_some(),
        ingress_routing: false,
        env_projection: !request.env.is_empty(),
        secret_projection: false,
        uid_gid_control: request.user.is_some() || mounts.iter().any(|m| m.ownership_init),
        readiness_probe: false,
        service_network_alias: !request.aliases.is_empty(),
    }
}

/// Error rendering an [`OciProjectionPlan`] into a runtime invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OciRenderError {
    /// A read-only mount also requested engine ownership init — a contradiction
    /// (`:U` requires write access). Carries the offending mount target.
    ReadOnlyOwnershipConflict { target: String },
}

/// The Podman `-v` option suffix for a mount, given its launch conditions.
///
/// * writable, no ownership → `""`
/// * read-only, no ownership → `":ro"`
/// * writable, ownership init → `":U"` (Podman engine-delegated UID/GID, #428)
///
/// Caller must reject `readonly && ownership_init` before rendering.
pub(crate) fn podman_mount_opts(readonly: bool, ownership_init: bool) -> &'static str {
    match (readonly, ownership_init) {
        (true, false) => ":ro",
        (false, true) => ":U",
        _ => "",
    }
}

/// Render a mount projection into the value passed after `-v` to `podman create`.
///
/// Bind paths are canonicalized to resolve symlinks (e.g. `/tmp` →
/// `/private/tmp`), falling back to the original on failure; engine-managed
/// volume names are passed verbatim (never canonicalized — they are not paths).
/// See #444.
pub(crate) fn render_podman_mount_value(mount: &OciMountProjection) -> String {
    let source = if mount.engine_volume {
        mount.source.clone()
    } else {
        std::fs::canonicalize(&mount.source)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| mount.source.clone())
    };
    format!(
        "{}:{}{}",
        source,
        mount.target,
        podman_mount_opts(mount.readonly, mount.ownership_init)
    )
}

/// Render a port projection into the value passed after `-p` to `podman create`.
///
/// Podman's `-p` grammar: `[[ip:][hostPort]:]containerPort[/protocol]`.
pub(crate) fn render_podman_port_value(port: &OciPortProjection) -> String {
    let suffix = format!("{}/{}", port.container_port, port.protocol);
    match (port.host_ip.as_deref(), port.host_port) {
        (Some(ip), Some(hp)) => format!("{ip}:{hp}:{suffix}"),
        (Some(ip), None) => format!("{ip}::{suffix}"),
        (None, Some(hp)) => format!("{hp}:{suffix}"),
        (None, None) => suffix,
    }
}

impl OciProjectionPlan {
    /// Render the derived `podman create` argv from this plan.
    ///
    /// This is the only place the runtime invocation is produced for the Podman
    /// provider: argv is *derived from* the plan, it is never the source of
    /// truth. `host` is the host platform used to decide whether `--platform`
    /// emulation is needed; it is passed in (not read from the environment) so
    /// rendering is deterministic and testable.
    ///
    /// The `--connection` flag (machine selection) is injected separately by the
    /// provider's command builder and is host state, not a plan field.
    pub(crate) fn render_podman_create_argv(
        &self,
        host: &OciPlatform,
    ) -> Result<Vec<String>, OciRenderError> {
        let mut args: Vec<String> = vec!["create".into(), "--name".into(), self.name.clone()];

        // --platform only when creating an emulated (non-native) container.
        if let Some(platform) = &self.platform {
            if platform.architecture != host.architecture {
                args.push("--platform".into());
                args.push(format!("linux/{}", platform.architecture));
            }
        }
        // Render both label sets (launch + provider bookkeeping) merged into a
        // single sorted sequence, so the emitted `--label` flags are equivalent
        // to the prior single-map behavior. The split only affects *identity*
        // (canonical_identity), never what podman is told.
        let all_labels: BTreeMap<&String, &String> = self
            .launch_labels
            .iter()
            .chain(self.provider_metadata_labels.iter())
            .collect();
        for (k, v) in &all_labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        for (k, v) in &self.env_projection {
            args.push("--env".into());
            args.push(format!("{k}={v}"));
        }
        for port in &self.ports {
            args.push("-p".into());
            args.push(render_podman_port_value(port));
        }
        if let Some(wd) = &self.cwd {
            args.push("--workdir".into());
            args.push(wd.clone());
        }
        if let Some(user) = &self.user {
            args.push("--user".into());
            args.push(user.clone());
        }
        for mount in &self.mounts {
            // readonly + ownership is a contradiction: a read-only mount cannot
            // be re-owned by the engine (:U requires write access).
            if mount.readonly && mount.ownership_init {
                return Err(OciRenderError::ReadOnlyOwnershipConflict {
                    target: mount.target.clone(),
                });
            }
            args.push("-v".into());
            args.push(render_podman_mount_value(mount));
        }
        if let Some(net) = &self.network.network {
            args.push("--network".into());
            args.push(net.clone());
        }
        for alias in &self.network.service_aliases {
            args.push("--network-alias".into());
            args.push(alias.clone());
        }
        for host_entry in &self.network.extra_hosts {
            args.push("--add-host".into());
            args.push(host_entry.clone());
        }
        args.push(self.image.reference.clone());
        args.extend(self.argv.iter().cloned());
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::runtime::oci::{OciMountSourceKind, OciMountSpec, OciPortSpec};
    use capsule_core::types::{MountOwnership, OciPlatform};
    use std::collections::HashMap;

    fn base_request() -> OciContainerRequest {
        OciContainerRequest {
            name: "ato-sess-abc123-web".to_string(),
            image: "docker.io/library/nginx:1.27".to_string(),
            cmd: vec!["nginx".into(), "-g".into(), "daemon off;".into()],
            env: HashMap::from([("PORT".to_string(), "8080".to_string())]),
            working_dir: Some("/app".to_string()),
            labels: HashMap::from([("ato.session".to_string(), "abc123".to_string())]),
            mounts: vec![OciMountSpec {
                source: "/srv/data".to_string(),
                target: "/data".to_string(),
                readonly: false,
                ownership: None,
                source_kind: OciMountSourceKind::BindPath,
            }],
            ports: vec![OciPortSpec {
                container_port: 8080,
                host_port: None,
                protocol: "tcp".to_string(),
                host_ip: Some("127.0.0.1".to_string()),
            }],
            network: Some("ato-net-abc123".to_string()),
            aliases: vec!["web".to_string()],
            platform: None,
            extra_hosts: vec!["host.containers.internal:host-gateway".to_string()],
            user: Some("1000:1000".to_string()),
        }
    }

    // ── #493: receipt-safe provider evidence derived from the #516 boundary ──

    #[test]
    fn oci_provider_projection_evidence_is_not_container_started_only() {
        let ev = OciProjectionPlan::from_container_request(&base_request()).receipt_evidence();
        assert_eq!(ev.provider_kind, "oci");
        assert_eq!(ev.provider_name, "podman");
        assert_eq!(ev.image_reference, "docker.io/library/nginx:1.27");
        // tag-only → unpinned (honest), not fabricated as resolved.
        assert!(matches!(
            ev.image_digest_status,
            OciImageDigestStatus::Unpinned
        ));
        assert_eq!(ev.env_keys, vec!["PORT".to_string()]);
        assert_eq!(ev.mounts.len(), 1);
        assert_eq!(ev.mounts[0].target, "/data");
        assert_eq!(ev.ports.len(), 1);
        assert_eq!(ev.ports[0].container_port, 8080);
        assert_eq!(ev.ports[0].protocol, "tcp");
        assert_eq!(ev.network_aliases, vec!["web".to_string()]);
        // Real provider detail — not merely "container started".
        assert!(!ev.capabilities_required.is_empty());
        assert!(
            ev.capabilities_required
                .contains(&"env-projection".to_string())
        );
        assert!(
            ev.capabilities_required
                .contains(&"network-policy".to_string())
        );

        // A pinned image surfaces the digest as evidence.
        let mut req = base_request();
        req.image = format!("repo/app@sha256:{}", "a".repeat(64));
        let ev = OciProjectionPlan::from_container_request(&req).receipt_evidence();
        match ev.image_digest_status {
            OciImageDigestStatus::Pinned { digest } => {
                assert_eq!(digest, format!("sha256:{}", "a".repeat(64)))
            }
            OciImageDigestStatus::Unpinned => panic!("expected pinned digest evidence"),
        }
    }

    #[test]
    fn oci_session_local_provider_metadata_does_not_enter_identity() {
        // The #516 label split keeps provider bookkeeping out of identity; the
        // receipt evidence inherits that — changing session-local metadata must
        // not change the evidence.
        let with_session = |session: &str, exec: &str| {
            let mut req = base_request();
            req.labels
                .insert("io.ato.session_id".into(), session.into());
            req.labels.insert("io.ato.execution_id".into(), exec.into());
            req.labels.insert("io.ato.managed".into(), "true".into());
            req.labels.insert("io.ato.provider".into(), "podman".into());
            OciProjectionPlan::from_container_request(&req).receipt_evidence()
        };
        let ev_a = with_session("session-a", "exec-a");
        let ev_b = with_session("session-b", "exec-b");
        assert_eq!(
            ev_a, ev_b,
            "session-local provider metadata must not change receipt evidence"
        );

        let json = serde_json::to_string(&ev_a).expect("encode");
        assert!(!json.contains("session-a"));
        assert!(!json.contains("exec-a"));
        // Container name and argv are session-local; they must not appear.
        assert!(!json.contains("ato-sess-abc123-web"));
        assert!(!json.contains("daemon off"));
    }

    #[test]
    fn receipt_does_not_leak_secret_or_env_values() {
        let mut req = base_request();
        req.env
            .insert("OPENAI_API_KEY".into(), "sk-test-secret".into());
        req.env.insert(
            "DATABASE_URL".into(),
            "postgres://user:password@host/db".into(),
        );
        let ev = OciProjectionPlan::from_container_request(&req).receipt_evidence();

        // Env var NAMES are recorded...
        assert!(ev.env_keys.contains(&"OPENAI_API_KEY".to_string()));
        assert!(ev.env_keys.contains(&"DATABASE_URL".to_string()));
        // ...but the raw VALUES never appear in the serialized receipt evidence.
        let json = serde_json::to_string(&ev).expect("encode");
        assert!(
            !json.contains("sk-test-secret"),
            "secret value leaked: {json}"
        );
        assert!(!json.contains("password"), "db password leaked: {json}");
        assert!(!json.contains("postgres://"), "db url leaked: {json}");
    }

    // ── #501: typed provider enforcement status + redacted derived command ──

    #[test]
    fn oci_provider_evidence_records_image_digest_status() {
        // Tag-only → unpinned (honest), pinned digest surfaces as evidence.
        let ev = OciProjectionPlan::from_container_request(&base_request()).receipt_evidence();
        assert!(matches!(
            ev.image_digest_status,
            OciImageDigestStatus::Unpinned
        ));
        assert_eq!(ev.provider_version.as_deref(), Some("oci-podman-v1"));

        let mut req = base_request();
        req.image = format!("repo/app@sha256:{}", "b".repeat(64));
        let ev = OciProjectionPlan::from_container_request(&req).receipt_evidence();
        assert!(matches!(
            ev.image_digest_status,
            OciImageDigestStatus::Pinned { .. }
        ));
    }

    #[test]
    fn oci_provider_evidence_records_enforcement_status() {
        let ev = OciProjectionPlan::from_container_request(&base_request()).receipt_evidence_with(
            OciEnforcementStatus::Unsupported,
            OciEnforcementStatus::Enforced,
        );
        assert_eq!(
            ev.network_enforcement_status,
            OciEnforcementStatus::Unsupported
        );
        assert_eq!(
            ev.capability_enforcement_status,
            OciEnforcementStatus::Enforced
        );
        let json = serde_json::to_string(&ev).expect("encode");
        // Typed + visible in the serialized receipt evidence.
        assert!(
            json.contains("unsupported"),
            "network enforcement status: {json}"
        );
    }

    #[test]
    fn oci_provider_evidence_redacts_derived_argv() {
        let mut req = base_request();
        req.env
            .insert("OPENAI_API_KEY".into(), "sk-secret-xyz".into());
        let ev = OciProjectionPlan::from_container_request(&req).receipt_evidence();

        // The derived command is present as evidence — flags survive, every
        // value (including positional subcommands like `create`) is redacted. It
        // is NEVER the raw command.
        assert!(!ev.derived_command_redacted.is_empty());
        assert!(ev.derived_command_redacted.contains(&"--env".to_string()));
        assert!(ev.derived_command_redacted.contains(&"--name".to_string()));
        let argv_json = serde_json::to_string(&ev.derived_command_redacted).expect("encode");
        // No raw value survives: not the env value/secret, container name, or
        // command tail.
        assert!(
            argv_json.contains("<redacted>"),
            "values must be redacted: {argv_json}"
        );
        assert!(
            !argv_json.contains("sk-secret-xyz"),
            "secret leaked: {argv_json}"
        );
        assert!(!argv_json.contains("8080"), "env value leaked: {argv_json}");
        assert!(
            !argv_json.contains("daemon off"),
            "argv value leaked: {argv_json}"
        );
        assert!(
            !argv_json.contains("ato-sess-abc123-web"),
            "container name leaked: {argv_json}"
        );
    }

    #[test]
    fn oci_provider_evidence_excludes_container_id_pid_log_path_from_identity() {
        // The receipt evidence has no field for these at all; assert they cannot
        // appear in the serialized evidence nor the identity summary.
        let ev = OciProjectionPlan::from_container_request(&base_request()).receipt_evidence();
        let json = serde_json::to_string(&ev).expect("encode");
        for forbidden in [
            "container_id",
            "provider_pid",
            "log_path",
            "c0ffee",
            "424242",
        ] {
            assert!(
                !json.contains(forbidden),
                "evidence leaked {forbidden}: {json}"
            );
        }
    }

    #[test]
    fn oci_projection_receipt_does_not_treat_podman_argv_as_identity() {
        let mut req = base_request();
        req.env.insert("PORT".into(), "8080".into());
        let ev = OciProjectionPlan::from_container_request(&req).receipt_evidence();

        // The podman argv lives ONLY as redacted, value-free derived evidence —
        // never as an identity field. The receipt evidence carries no identity
        // summary at all (a hash of the value-bearing canonical identity would be
        // a correlation oracle); the image identity is a separate digest-status
        // field, not the argv.
        assert!(!ev.derived_command_redacted.is_empty());
        let argv_json = serde_json::to_string(&ev.derived_command_redacted).expect("encode");
        assert!(
            argv_json.contains("<redacted>"),
            "argv values must be redacted"
        );
        assert!(
            !argv_json.contains("PORT=8080"),
            "env value must not survive in argv"
        );

        // The whole serialized evidence never carries a raw `create … <image> …`
        // command line; only flags + redacted placeholders.
        let json = serde_json::to_string(&ev).expect("encode");
        assert!(
            !json.contains("daemon off"),
            "raw command tail must not appear: {json}"
        );
    }

    #[test]
    fn oci_projection_changes_when_launch_conditions_change() {
        let base = OciProjectionPlan::from_container_request(&base_request());
        let base_id = base.canonical_identity();

        // env change
        let mut req = base_request();
        req.env.insert("EXTRA".to_string(), "1".to_string());
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "env change must change the plan identity"
        );

        // mount change
        let mut req = base_request();
        req.mounts[0].target = "/data2".to_string();
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "mount change must change the plan identity"
        );

        // network policy change
        let mut req = base_request();
        req.network = Some("other-net".to_string());
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "network change must change the plan identity"
        );

        // entrypoint/argv change
        let mut req = base_request();
        req.cmd = vec!["sh".to_string()];
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "argv change must change the plan identity"
        );

        // cwd change
        let mut req = base_request();
        req.working_dir = Some("/other".to_string());
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "cwd change must change the plan identity"
        );

        // user change
        let mut req = base_request();
        req.user = Some("0:0".to_string());
        assert_ne!(
            base_id,
            OciProjectionPlan::from_container_request(&req).canonical_identity(),
            "user change must change the plan identity"
        );
    }

    #[test]
    fn oci_projection_identity_excludes_session_local_fields() {
        let plan = OciProjectionPlan::from_container_request(&base_request());
        let identity = plan.canonical_identity();

        // The requested container name (which embeds a session id) is a
        // rendering input, not part of the launch identity.
        assert!(
            !identity.contains("ato-sess-abc123-web"),
            "requested container name must not appear in projection identity"
        );

        // Runtime evidence lives in a separate struct and must never leak into
        // the plan identity. The plan type has no field for these at all.
        let evidence = super::super::ProviderProjectionEvidence {
            container_id: Some("c0ffee1234".to_string()),
            provider_pid: Some(424242),
            log_path: Some("/runs/abc123/web.log".to_string()),
        };
        assert!(!identity.contains(evidence.container_id.as_deref().unwrap()));
        assert!(!identity.contains("424242"));
        assert!(!identity.contains(evidence.log_path.as_deref().unwrap()));

        // Changing the requested name does not change identity (it is excluded).
        let mut renamed = base_request();
        renamed.name = "ato-sess-zzz999-web".to_string();
        assert_eq!(
            identity,
            OciProjectionPlan::from_container_request(&renamed).canonical_identity(),
            "session-local name must not affect projection identity"
        );
    }

    #[test]
    fn oci_projection_identity_excludes_session_local_labels() {
        // The OCI executors stamp `io.ato.session_id` (and friends) onto every
        // container as provider bookkeeping. Two launches of the same Capsule in
        // different sessions differ only by these labels — and must therefore
        // share one projection identity.
        let mut req = base_request();
        req.labels
            .insert("io.ato.session_id".into(), "session-a".into());
        let a = OciProjectionPlan::from_container_request(&req).canonical_identity();

        req.labels
            .insert("io.ato.session_id".into(), "session-b".into());
        let b = OciProjectionPlan::from_container_request(&req).canonical_identity();

        assert_eq!(a, b, "session id label must not affect projection identity");
        assert!(!a.contains("session-a"));
        assert!(!b.contains("session-b"));

        // The same applies to the other provider-bookkeeping labels.
        let mut req = base_request();
        req.labels.insert("io.ato.managed".into(), "true".into());
        req.labels
            .insert("io.ato.execution_id".into(), "exec-xyz".into());
        req.labels.insert("io.ato.provider".into(), "podman".into());
        let with_meta = OciProjectionPlan::from_container_request(&req).canonical_identity();
        let baseline =
            OciProjectionPlan::from_container_request(&base_request()).canonical_identity();
        assert_eq!(with_meta, baseline);
        assert!(!with_meta.contains("exec-xyz"));

        // But these labels are still rendered to `podman create --label`, so the
        // provider can find and reap its own containers (behavior parity).
        let plan = OciProjectionPlan::from_container_request(&req);
        let argv = plan.render_podman_create_argv(&host_arm64()).unwrap();
        let labels = all_args_after(&argv, "--label");
        assert!(labels.contains(&"io.ato.managed=true"));
        assert!(labels.contains(&"io.ato.execution_id=exec-xyz"));
        assert!(labels.contains(&"io.ato.provider=podman"));
        // ...and the launch label is still rendered too.
        assert!(labels.contains(&"ato.session=abc123"));

        // `io.ato.target` is a launch condition, not bookkeeping: it DOES affect
        // identity.
        let mut req_a = base_request();
        req_a.labels.insert("io.ato.target".into(), "app".into());
        let mut req_b = base_request();
        req_b.labels.insert("io.ato.target".into(), "db".into());
        assert_ne!(
            OciProjectionPlan::from_container_request(&req_a).canonical_identity(),
            OciProjectionPlan::from_container_request(&req_b).canonical_identity(),
            "io.ato.target is a launch condition and must affect identity"
        );
    }

    #[test]
    fn oci_image_digest_is_not_capsule_identity() {
        // A pinned reference records the digest as image evidence...
        let pinned = format!("repo/app@sha256:{}", "a".repeat(64));
        let plan = OciProjectionPlan::from_container_request(&OciContainerRequest {
            image: pinned.clone(),
            ..base_request()
        });
        assert!(plan.image.digest.is_pinned());
        match &plan.image.digest {
            OciImageDigest::Pinned(d) => assert_eq!(d, &format!("sha256:{}", "a".repeat(64))),
            OciImageDigest::Unpinned => panic!("expected pinned digest"),
        }

        // ...but the digest is evidence under `image`, not a standalone Capsule
        // identity. Two plans differing only by image still differ by their
        // *reference*, and the digest never stands alone as the identity.
        let identity = plan.canonical_identity();
        assert!(identity.contains("image.reference="));
        assert!(identity.contains("image.digest=pinned("));
        // The digest line is explicitly labeled as digest evidence, never as a
        // bare "capsule identity = <digest>".
        assert!(!identity.contains(&format!("identity={pinned}")));
    }

    #[test]
    fn oci_tag_only_image_is_unpinned_not_silently_resolved() {
        let plan = OciProjectionPlan::from_container_request(&OciContainerRequest {
            image: "docker.io/library/redis:7".to_string(),
            ..base_request()
        });
        assert_eq!(plan.image.digest, OciImageDigest::Unpinned);
        assert!(!plan.image.digest.is_pinned());
        assert!(plan.canonical_identity().contains("image.digest=unpinned"));

        // A malformed digest is also treated as unpinned, not accepted blindly.
        let plan = OciProjectionPlan::from_container_request(&OciContainerRequest {
            image: "repo/app@sha256:tooshort".to_string(),
            ..base_request()
        });
        assert_eq!(plan.image.digest, OciImageDigest::Unpinned);
    }

    #[test]
    fn required_capabilities_reflect_launch_conditions() {
        let mut req = base_request();
        // Engine-managed persistent state binding + ownership init.
        req.mounts = vec![OciMountSpec {
            source: "ato-state-abc-pg".to_string(),
            target: "/var/lib/postgresql/data".to_string(),
            readonly: false,
            ownership: Some(MountOwnership::default()),
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: false,
            },
        }];
        let plan = OciProjectionPlan::from_container_request(&req);
        let caps = plan.capabilities_required;
        assert!(caps.filesystem_projection);
        assert!(caps.persistent_state);
        assert!(caps.uid_gid_control); // user set + ownership init
        assert!(caps.network_policy);
        assert!(caps.env_projection);
        assert!(caps.service_network_alias);
        assert!(!caps.read_only_mounts);
        // Not determinable at this layer — must not be guessed.
        assert!(!caps.ingress_routing);
        assert!(!caps.readiness_probe);

        assert_eq!(plan.state_bindings().count(), 1);
        assert_eq!(plan.service_edges(), &["web".to_string()]);
    }

    #[test]
    fn platform_override_is_carried_into_plan() {
        let mut req = base_request();
        req.platform = Some(OciPlatform {
            os: "linux".to_string(),
            architecture: "amd64".to_string(),
            variant: None,
        });
        let plan = OciProjectionPlan::from_container_request(&req);
        assert_eq!(plan.platform.as_ref().unwrap().architecture, "amd64");
        assert!(plan.canonical_identity().contains("amd64"));
    }

    fn host_arm64() -> OciPlatform {
        OciPlatform {
            os: "linux".to_string(),
            architecture: "arm64".to_string(),
            variant: None,
        }
    }

    /// Find the value immediately following the first occurrence of `flag`.
    fn arg_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .map(String::as_str)
    }

    fn all_args_after<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
        argv.iter()
            .enumerate()
            .filter(|(_, a)| a.as_str() == flag)
            .filter_map(|(i, _)| argv.get(i + 1))
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn oci_projection_plan_renders_existing_invocation() {
        let plan = OciProjectionPlan::from_container_request(&base_request());
        let argv = plan.render_podman_create_argv(&host_arm64()).unwrap();

        // Prefix and image/argv tail match the previous create_container shape.
        assert_eq!(&argv[0..3], &["create", "--name", "ato-sess-abc123-web"]);
        let image_pos = argv
            .iter()
            .position(|a| a == "docker.io/library/nginx:1.27")
            .expect("image must appear");
        assert_eq!(
            &argv[image_pos + 1..],
            &["nginx", "-g", "daemon off;"],
            "command/argv follows the image, unchanged"
        );

        // Flag groups match the previous invocation (compared as values so env
        // ordering, now deterministic, does not make this brittle).
        assert_eq!(all_args_after(&argv, "--env"), vec!["PORT=8080"]);
        assert_eq!(all_args_after(&argv, "--label"), vec!["ato.session=abc123"]);
        assert_eq!(arg_after(&argv, "--workdir"), Some("/app"));
        assert_eq!(arg_after(&argv, "--user"), Some("1000:1000"));
        assert_eq!(arg_after(&argv, "-p"), Some("127.0.0.1::8080/tcp"));
        assert_eq!(arg_after(&argv, "--network"), Some("ato-net-abc123"));
        assert_eq!(arg_after(&argv, "--network-alias"), Some("web"));
        assert_eq!(
            arg_after(&argv, "--add-host"),
            Some("host.containers.internal:host-gateway")
        );
        // Bind mount: target preserved, writable (no opts). Source may be
        // canonicalized; assert on the stable `:target` tail.
        assert!(
            all_args_after(&argv, "-v")[0].ends_with(":/data"),
            "mount renders <source>:/data with no opts"
        );

        // --platform is omitted when the plan has no override (native launch).
        assert!(!argv.iter().any(|a| a == "--platform"));
    }

    #[test]
    fn oci_projection_plan_is_source_of_truth_not_command_string() {
        // The argv is *derived* from plan fields: mutate the plan and the
        // rendered command changes accordingly — the command string is never
        // the source of truth, the plan is.
        let mut plan = OciProjectionPlan::from_container_request(&base_request());

        let before = plan.render_podman_create_argv(&host_arm64()).unwrap();
        assert_eq!(arg_after(&before, "--user"), Some("1000:1000"));

        // Change a plan field; re-render must reflect it.
        plan.user = Some("0:0".to_string());
        plan.env_projection
            .insert("NEW".to_string(), "v".to_string());
        let after = plan.render_podman_create_argv(&host_arm64()).unwrap();
        assert_eq!(arg_after(&after, "--user"), Some("0:0"));
        assert!(all_args_after(&after, "--env").contains(&"NEW=v"));

        // Rendering is deterministic (a pure projection of plan fields): two
        // renders of the same plan are byte-for-byte identical.
        assert_eq!(
            after,
            plan.render_podman_create_argv(&host_arm64()).unwrap()
        );

        // A platform override different from the host renders --platform; equal
        // to the host it does not (emulation only when needed).
        plan.platform = Some(OciPlatform {
            os: "linux".to_string(),
            architecture: "amd64".to_string(),
            variant: None,
        });
        let emulated = plan.render_podman_create_argv(&host_arm64()).unwrap();
        assert_eq!(arg_after(&emulated, "--platform"), Some("linux/amd64"));
        let native = plan
            .render_podman_create_argv(&OciPlatform {
                os: "linux".to_string(),
                architecture: "amd64".to_string(),
                variant: None,
            })
            .unwrap();
        assert!(!native.iter().any(|a| a == "--platform"));
    }

    #[test]
    fn render_rejects_readonly_ownership_conflict() {
        let mut req = base_request();
        req.mounts = vec![OciMountSpec {
            source: "/srv/ro".to_string(),
            target: "/ro".to_string(),
            readonly: true,
            ownership: Some(MountOwnership::default()),
            source_kind: OciMountSourceKind::BindPath,
        }];
        let plan = OciProjectionPlan::from_container_request(&req);
        assert_eq!(
            plan.render_podman_create_argv(&host_arm64()),
            Err(OciRenderError::ReadOnlyOwnershipConflict {
                target: "/ro".to_string()
            })
        );
    }

    #[test]
    fn render_engine_volume_source_is_verbatim() {
        let mut req = base_request();
        req.mounts = vec![OciMountSpec {
            source: "ato-state-deadbeef-data".to_string(),
            target: "/data".to_string(),
            readonly: false,
            ownership: Some(MountOwnership::default()),
            source_kind: OciMountSourceKind::EngineVolume {
                remove_on_stop: false,
            },
        }];
        let plan = OciProjectionPlan::from_container_request(&req);
        let argv = plan.render_podman_create_argv(&host_arm64()).unwrap();
        assert_eq!(
            all_args_after(&argv, "-v"),
            vec!["ato-state-deadbeef-data:/data:U"],
            "engine volume name is passed verbatim with :U ownership init"
        );
    }
}
