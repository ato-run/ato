//! The launch contract between the control plane and a Runner executor.
//!
//! Three layers, deliberately distinct:
//!
//! 1. **ComputeSchema** — the immutable execution definition. Not here.
//! 2. [`RuntimeLaunchSpecV1`] — the serializable LOGICAL projection of a
//!    schema, instance, bindings and state onto something an executor can act
//!    on. This is what crosses the wire.
//! 3. `ResolvedRuntimeLaunchContext` — physical paths, secret values and
//!    allocated ports. Runner-local, never serialized. See
//!    [`crate::runtime_launch::resolved`].
//!
//! The split is the point. A logical spec can be logged, digested, stored on a
//! Run receipt and read by an operator; a resolved context cannot, because
//! doing so would publish exactly the things that must not leave the host.
//!
//! Process and OCI are two realizations of ONE contract. They differ in the
//! `realization` arm and nowhere else, so state, endpoints, readiness and
//! lifecycle cannot drift into two dialects.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Wire discriminator. A Runner that does not recognise it must refuse the
/// launch rather than guess, so an older Runner can never silently ignore a
/// field a newer control plane depended on.
pub const RUNTIME_LAUNCH_SPEC_V1_PROTOCOL: &str = "ato.runtime-launch-spec.v1";

/// `runner_leases.command_json` envelope kind carrying this spec. It rides in
/// the EXISTING dispatch envelope beside `run_capsule` and the activity
/// executor kinds; there is no second wire protocol.
pub const RUNTIME_LAUNCH_SPEC_LEASE_KIND: &str = "runtime_launch_spec_v1";

/// Typed refusals. Each names the invariant, not the field, so the same code
/// means the same thing to the API, the Runner and an operator reading a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLaunchSpecError {
    /// The producer speaks a protocol this build cannot honour.
    UnsupportedVersion { found: String },
    /// `argv` is empty, so there is nothing to execute.
    EmptyArgv,
    /// `cwd` is absolute, escapes the workspace, or is not normalizable.
    InvalidCwd { cwd: String },
    /// An environment name is declared twice, or as both public and secret.
    EnvConflict { name: String },
    /// Two state attachments claim the same key or the same mount target.
    MountConflict { target: String },
    /// A state key appears twice.
    StateKeyConflict { key: String },
    /// A mount target is relative, or not normalizable.
    InvalidMountTarget { target: String },
    /// Readiness names an endpoint that is not declared.
    InvalidReadiness { endpoint: String },
    /// A payload field carried something that must never cross the wire.
    ForbiddenField { field: String },
    /// An endpoint name is declared twice.
    EndpointConflict { name: String },
    /// An identity or reference that everything else keys off is empty.
    EmptyIdentity { field: String },
    /// An OCI reference is not content-addressed.
    InvalidImageDigest { reference: String },
    /// An endpoint's allocation and its ports disagree.
    InvalidEndpoint { name: String },
    /// A timeout is zero, or shutdown bounds are inconsistent.
    InvalidLifecycle { field: String },
}

impl RuntimeLaunchSpecError {
    /// Stable `ATO_ERR_*` code for logs, receipts and API responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedVersion { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_UNSUPPORTED_VERSION",
            Self::EmptyArgv => "ATO_ERR_RUNTIME_LAUNCH_SPEC_EMPTY_ARGV",
            Self::InvalidCwd { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_CWD",
            Self::EnvConflict { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_ENV_CONFLICT",
            Self::MountConflict { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_MOUNT_CONFLICT",
            Self::StateKeyConflict { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_STATE_KEY_CONFLICT",
            Self::InvalidMountTarget { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_MOUNT_TARGET",
            Self::InvalidReadiness { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_READINESS",
            Self::ForbiddenField { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_FORBIDDEN_FIELD",
            Self::EndpointConflict { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_ENDPOINT_CONFLICT",
            Self::EmptyIdentity { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_EMPTY_IDENTITY",
            Self::InvalidImageDigest { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_IMAGE_DIGEST",
            Self::InvalidEndpoint { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_ENDPOINT",
            Self::InvalidLifecycle { .. } => "ATO_ERR_RUNTIME_LAUNCH_SPEC_INVALID_LIFECYCLE",
        }
    }
}

impl std::fmt::Display for RuntimeLaunchSpecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.code())
    }
}

impl std::error::Error for RuntimeLaunchSpecError {}

/// Identity carried for observability and correlation ONLY.
///
/// None of these is an executor's physical identity: a container id or pid is
/// the realization's, and binding a ComputeInstance to one would make the
/// instance die with the process it happens to be running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchContextV1 {
    pub run_id: String,
    pub compute_id: String,
    pub compute_schema_id: String,
    pub compute_instance_id: String,
}

/// Where the executor starts from. `cwd_relative` is resolved against the
/// materialized workspace root ON THE RUNNER; the control plane never learns
/// that path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchWorkspaceV1 {
    /// Content reference of the schema's materialization.
    pub materialization_ref: String,
    /// Workspace-relative. `""` means the workspace root.
    #[serde(default)]
    pub cwd_relative: String,
}

/// The realization arm. Everything outside it is shared by both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchRealizationV1 {
    Process(ProcessRealizationV1),
    Oci(OciRealizationV1),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRealizationV1 {
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciRealizationV1 {
    /// A content-addressed DIGEST, `sha256:<64 hex>`.
    ///
    /// A mutable tag (`python:latest`) cannot establish identity: the same
    /// spec would launch different code on different days, and its digest
    /// would name something that is not reproducible. The control plane
    /// resolves the tag before building the spec, and this is validated.
    pub image_digest_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

/// A non-secret environment variable, carried by value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicEnvV1 {
    pub name: String,
    pub value: String,
}

/// A secret environment variable, carried by REFERENCE.
///
/// There is no `value` field, and adding one would be a wire-level security
/// regression: this struct is `deny_unknown_fields`, so a payload that carries
/// a value is refused rather than silently accepted and logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretGrantV1 {
    pub name: String,
    /// Opaque to the Runner until it redeems it at the spawn boundary.
    pub grant_ref: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateAccessV1 {
    ReadOnly,
    ReadWrite,
}

/// A request to attach durable state. Logical only: which revision, and where
/// it should appear inside the guest. The working-copy path that satisfies it
/// is created on the Runner and never travels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateAttachmentV1 {
    pub state_key: String,
    /// `null` for an instance that has never been written.
    ///
    /// Always serialized, including when null: this field is semantically
    /// nullable rather than optional, and skipping it would make Rust and
    /// TypeScript canonicalize the SAME spec into different bytes — which the
    /// cross-language fixtures exist to prevent.
    #[serde(default)]
    pub revision_ref: Option<String>,
    /// Absolute path INSIDE the guest, e.g. `/data`.
    pub mount_target: String,
    pub access: StateAccessV1,
    /// A NON-SECRET monotonic fence identifying the current writer generation.
    ///
    /// Deliberately not a capability. This spec is designed to be persisted
    /// and digested onto a Run receipt, so a bearer token here would publish
    /// an authorization secret. Authorization is the authenticated Runner plus
    /// the assigned Run; this value only lets a commit be REFUSED when a newer
    /// writer has since taken the slot.
    ///
    /// P2 populates it. Absent means no writer generation has been assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_fence: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointAllocationV1 {
    Automatic,
    Preferred,
}

/// A logical endpoint. The control plane names it and may state the port the
/// workload listens on INSIDE the guest; it never picks the host port, because
/// only the Runner knows what is free and the stable URL must not depend on
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointV1 {
    pub name: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_port: Option<u16>,
    pub allocation: EndpointAllocationV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadinessV1 {
    Http {
        endpoint_name: String,
        path: String,
        timeout_ms: u64,
    },
    Tcp {
        endpoint_name: String,
        timeout_ms: u64,
    },
    /// The workload is ready once its process is up. Weakest useful signal;
    /// only correct where the workload has no reachable endpoint.
    Process { timeout_ms: u64 },
}

impl ReadinessV1 {
    fn timeout_ms(&self) -> u64 {
        match self {
            Self::Http { timeout_ms, .. }
            | Self::Tcp { timeout_ms, .. }
            | Self::Process { timeout_ms } => *timeout_ms,
        }
    }

    fn endpoint_name(&self) -> Option<&str> {
        match self {
            Self::Http { endpoint_name, .. } | Self::Tcp { endpoint_name, .. } => {
                Some(endpoint_name)
            }
            Self::Process { .. } => None,
        }
    }
}

/// How a Run ends. Both bounds are explicit so a workload that ignores its
/// signal cannot hold a Runner slot open indefinitely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleV1 {
    pub graceful_shutdown_ms: u64,
    pub force_kill_after_ms: u64,
}

/// The logical launch contract. Serializable, digestible, safe to persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLaunchSpecV1 {
    pub protocol: String,
    pub context: LaunchContextV1,
    pub workspace: LaunchWorkspaceV1,
    pub realization: LaunchRealizationV1,
    #[serde(default)]
    pub public_env: Vec<PublicEnvV1>,
    #[serde(default)]
    pub secret_grants: Vec<SecretGrantV1>,
    #[serde(default)]
    pub state_attachments: Vec<StateAttachmentV1>,
    #[serde(default)]
    pub endpoints: Vec<EndpointV1>,
    pub readiness: ReadinessV1,
    pub lifecycle: LifecycleV1,
}

impl RuntimeLaunchSpecV1 {
    /// Every invariant an executor is entitled to assume.
    ///
    /// Run at BOTH ends on purpose: the control plane must not emit a spec it
    /// knows is invalid, and the Runner must not trust that it didn't.
    pub fn validate(&self) -> Result<(), RuntimeLaunchSpecError> {
        if self.protocol != RUNTIME_LAUNCH_SPEC_V1_PROTOCOL {
            return Err(RuntimeLaunchSpecError::UnsupportedVersion {
                found: self.protocol.clone(),
            });
        }

        // Everything downstream keys off these. An empty one is not a
        // degraded launch, it is an unattributable one — no receipt, no
        // correlation, no way to tell whose state was touched.
        for (field, value) in [
            ("context.run_id", &self.context.run_id),
            ("context.compute_id", &self.context.compute_id),
            ("context.compute_schema_id", &self.context.compute_schema_id),
            (
                "context.compute_instance_id",
                &self.context.compute_instance_id,
            ),
            (
                "workspace.materialization_ref",
                &self.workspace.materialization_ref,
            ),
        ] {
            if value.is_empty() {
                return Err(RuntimeLaunchSpecError::EmptyIdentity {
                    field: field.to_owned(),
                });
            }
        }

        match &self.realization {
            LaunchRealizationV1::Process(process) => {
                if process.argv.is_empty() || process.argv[0].is_empty() {
                    return Err(RuntimeLaunchSpecError::EmptyArgv);
                }
            }
            LaunchRealizationV1::Oci(oci) => {
                if !is_content_addressed_digest(&oci.image_digest_ref) {
                    return Err(RuntimeLaunchSpecError::InvalidImageDigest {
                        reference: oci.image_digest_ref.clone(),
                    });
                }
                if let Some(argv) = &oci.argv
                    && (argv.is_empty() || argv[0].is_empty())
                {
                    return Err(RuntimeLaunchSpecError::EmptyArgv);
                }
            }
        }

        validate_workspace_relative(&self.workspace.cwd_relative)?;

        let mut names = BTreeSet::new();
        for env in &self.public_env {
            if env.name.is_empty() || !names.insert(env.name.as_str()) {
                return Err(RuntimeLaunchSpecError::EnvConflict {
                    name: env.name.clone(),
                });
            }
        }
        // Secrets share ONE namespace with public env: a name in both would
        // resolve to whichever the executor happened to apply last, and that
        // ambiguity is how a secret gets replaced by a public value.
        for grant in &self.secret_grants {
            if grant.name.is_empty() || !names.insert(grant.name.as_str()) {
                return Err(RuntimeLaunchSpecError::EnvConflict {
                    name: grant.name.clone(),
                });
            }
            if grant.grant_ref.is_empty() {
                return Err(RuntimeLaunchSpecError::ForbiddenField {
                    field: format!("secret_grants[{}].grant_ref", grant.name),
                });
            }
        }

        let mut state_keys = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for attachment in &self.state_attachments {
            if attachment.state_key.is_empty() || !state_keys.insert(attachment.state_key.as_str())
            {
                return Err(RuntimeLaunchSpecError::StateKeyConflict {
                    key: attachment.state_key.clone(),
                });
            }
            validate_mount_target(&attachment.mount_target)?;
            if !targets.insert(attachment.mount_target.as_str()) {
                return Err(RuntimeLaunchSpecError::MountConflict {
                    target: attachment.mount_target.clone(),
                });
            }
        }

        let mut endpoint_names = BTreeSet::new();
        for endpoint in &self.endpoints {
            if endpoint.name.is_empty() || !endpoint_names.insert(endpoint.name.as_str()) {
                return Err(RuntimeLaunchSpecError::EndpointConflict {
                    name: endpoint.name.clone(),
                });
            }
            // Allocation and ports must agree. `preferred` without a port is a
            // preference for nothing; `automatic` WITH one reads as a request
            // the Runner is free to ignore, and a caller that believed it was
            // honoured would build a URL against a port nobody bound.
            let consistent = match endpoint.allocation {
                EndpointAllocationV1::Preferred => {
                    matches!(endpoint.preferred_port, Some(port) if port != 0)
                }
                EndpointAllocationV1::Automatic => endpoint.preferred_port.is_none(),
            };
            if !consistent || matches!(endpoint.guest_port, Some(0)) {
                return Err(RuntimeLaunchSpecError::InvalidEndpoint {
                    name: endpoint.name.clone(),
                });
            }
        }

        if let Some(name) = self.readiness.endpoint_name() {
            let Some(endpoint) = self.endpoints.iter().find(|item| item.name == name) else {
                return Err(RuntimeLaunchSpecError::InvalidReadiness {
                    endpoint: name.to_owned(),
                });
            };
            // A probe needs somewhere to connect. An endpoint without a guest
            // port cannot be probed, so readiness would silently never fire.
            if endpoint.guest_port.is_none() {
                return Err(RuntimeLaunchSpecError::InvalidReadiness {
                    endpoint: name.to_owned(),
                });
            }
        }

        if self.readiness.timeout_ms() == 0 {
            return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                field: "readiness.timeout_ms".to_owned(),
            });
        }
        // Zero would mean "kill immediately", making the graceful bound a lie.
        if self.lifecycle.graceful_shutdown_ms == 0 {
            return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                field: "lifecycle.graceful_shutdown_ms".to_owned(),
            });
        }
        // The force bound must come strictly after the graceful one, or the
        // workload is killed before it has been asked to stop.
        if self.lifecycle.force_kill_after_ms <= self.lifecycle.graceful_shutdown_ms {
            return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                field: "lifecycle.force_kill_after_ms".to_owned(),
            });
        }

        Ok(())
    }
}

/// `sha256:<64 lowercase hex>`. A tag is refused: the same spec must always
/// name the same image, or its digest names something unreproducible.
fn is_content_addressed_digest(reference: &str) -> bool {
    let Some(hex) = reference.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// A cwd must stay inside the workspace. Absolute paths and `..` are refused
/// outright rather than normalized, because a spec that needed normalizing is
/// a spec whose author disagreed with the executor about what it meant.
fn validate_workspace_relative(cwd: &str) -> Result<(), RuntimeLaunchSpecError> {
    let invalid = || RuntimeLaunchSpecError::InvalidCwd {
        cwd: cwd.to_owned(),
    };
    if cwd.is_empty() {
        return Ok(());
    }
    if cwd.starts_with('/') || cwd.starts_with('\\') || cwd.contains('\0') {
        return Err(invalid());
    }
    // A Windows drive prefix is absolute too, and is not caught by the leading
    // separator check.
    if cwd.len() >= 2 && cwd.as_bytes()[1] == b':' {
        return Err(invalid());
    }
    for segment in cwd.split(['/', '\\']) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(invalid());
        }
    }
    Ok(())
}

/// A mount target is a guest path and must be absolute and normal. Anything
/// relative would depend on the executor's cwd, which differs between Process
/// and OCI — the one place the two realizations must not diverge.
fn validate_mount_target(target: &str) -> Result<(), RuntimeLaunchSpecError> {
    let invalid = || RuntimeLaunchSpecError::InvalidMountTarget {
        target: target.to_owned(),
    };
    if !target.starts_with('/') || target.contains('\0') || target.contains('\\') {
        return Err(invalid());
    }
    let mut segments = 0usize;
    for segment in target.split('/').skip(1) {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(invalid());
        }
        segments += 1;
    }
    if segments == 0 {
        // `/` itself: mounting state over the guest root is never intended.
        return Err(invalid());
    }
    Ok(())
}

impl RuntimeLaunchSpecV1 {
    /// RFC 8785 canonical bytes. Validated first, so a digest can only ever
    /// exist for a spec an executor would accept — a digest over an invalid
    /// spec would name something that can never run.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RuntimeLaunchSpecError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|_| RuntimeLaunchSpecError::ForbiddenField {
            field: "canonicalization".to_owned(),
        })
    }

    /// Stable identity of this launch, suitable for a Run receipt.
    ///
    /// Safe to persist precisely because the spec carries no secret value and
    /// no host path: there is nothing in the digest input that could not
    /// already be shown to the instance's owner.
    pub fn canonical_digest(&self) -> Result<String, RuntimeLaunchSpecError> {
        use sha2::{Digest, Sha256};
        let bytes = self.canonical_bytes()?;
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
    }

    /// Parses a spec from the `runner_leases.command_json` envelope, refusing
    /// anything this build cannot honour.
    pub fn parse(raw: &str) -> Result<Self, RuntimeLaunchSpecError> {
        let spec: Self =
            serde_json::from_str(raw).map_err(|error| RuntimeLaunchSpecError::ForbiddenField {
                field: format!("payload: {error}"),
            })?;
        spec.validate()?;
        Ok(spec)
    }
}

pub mod resolved;

#[cfg(test)]
mod tests;
