//! Launch contract v2: an OCI service group.
//!
//! v2 exists for exactly one reason — a route whose steps run as several
//! containers — and deliberately changes nothing else. It rides the SAME
//! `runtime_launch` lease kind as v1; `protocol` tells the two apart. The v1
//! type, bytes and digest are untouched, and a Runner build that does not know
//! v2 refuses it by `protocol` before anything is materialized.
//!
//! Everything a v1 spec says once, route-wide, that differs per service —
//! image, argv, environment, secrets, state, endpoints, readiness — moves into
//! each service. What stays route-wide is what one Run shares: its identity,
//! its workspace, its state slots and its lifecycle.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::oci_service_group::{
    OCI_SERVICE_GROUP_MAX_PORTS, OCI_SERVICE_GROUP_MAX_SERVICES, OCI_SERVICE_GROUP_MIN_SERVICES,
    group_totals, is_service_name,
};
use crate::runtime_launch::{
    LaunchContextV1, LaunchWorkspaceV1, LifecycleV1, OciResourceLimitsV1, PublicEnvV1,
    RUNTIME_LAUNCH_SPEC_V1_PROTOCOL, RuntimeLaunchSpecError, RuntimeLaunchSpecV1, SecretGrantV1,
    StateAttachmentV1, is_content_addressed_digest, is_guest_path, validate_mount_target,
};

use crate::runtime_launch_v3::{
    RUNTIME_LAUNCH_SPEC_V3_PROTOCOL, RunnerVolumeBackingV3, RuntimeLaunchSpecV3, StateAttachmentV3,
};

pub const RUNTIME_LAUNCH_SPEC_V2_PROTOCOL: &str = "ato.runtime-launch-spec.v2";

/// Surface Endpoint protocol. Matches the v1 endpoint vocabulary.
pub const SERVICE_ENDPOINT_HTTP: &str = "http";
/// Internal Endpoint protocol: payload-opaque TCP between sibling services.
pub const SERVICE_ENDPOINT_TCP: &str = "tcp";

fn group_error(field: impl Into<String>) -> RuntimeLaunchSpecError {
    RuntimeLaunchSpecError::InvalidServiceGroup {
        field: field.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLaunchSpecV2 {
    pub protocol: String,
    pub context: LaunchContextV1,
    /// `cwd_relative` must be empty: each service names its own working dir.
    pub workspace: LaunchWorkspaceV1,
    pub realization: LaunchRealizationV2,
    /// Slots of this Run. Each is mounted into exactly one service, named by
    /// that service's `state_keys`.
    pub state_attachments: Vec<StateAttachmentV1>,
    pub lifecycle: LifecycleV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchRealizationV2 {
    OciServiceGroup(OciServiceGroupV2),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciServiceGroupV2 {
    /// Shared by every service: the group runs on one Runner.
    pub platform: String,
    /// Authored order is start order; stop is the reverse.
    pub services: Vec<OciServiceV2>,
    /// The sum of every service's limits. Stated so the Runner can compare it
    /// with the CPU its lease reserved without re-deriving it.
    pub total_limits: OciResourceLimitsV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OciServiceV2 {
    /// Unique DNS label; the service's network alias for its siblings.
    pub name: String,
    pub image_digest_ref: String,
    /// Pullable `repository@<image_digest_ref>`.
    pub image_reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub argv: Vec<String>,
    /// Absolute guest path.
    pub working_dir: String,
    /// Read-only guest target for the materialized workspace.
    pub workspace_mount_path: String,
    pub resource_limits: OciResourceLimitsV1,
    pub public_env: Vec<PublicEnvV1>,
    /// Secrets for THIS service only. A sibling never receives them.
    pub secret_grants: Vec<SecretGrantV1>,
    /// Keys of `state_attachments` mounted into THIS service only.
    pub state_keys: Vec<String>,
    pub endpoints: Vec<ServiceEndpointV2>,
    pub readiness: ServiceReadinessV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointExposureV2 {
    /// Loopback-forwarded by the Runner to the Application Surface.
    Surface,
    /// Reachable only by sibling services; never forwarded, never listened on
    /// by the host.
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEndpointV2 {
    pub name: String,
    pub protocol: String,
    pub guest_port: u16,
    pub exposure: EndpointExposureV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceReadinessV2 {
    Http {
        endpoint_name: String,
        path: String,
        timeout_ms: u64,
    },
    Tcp {
        endpoint_name: String,
        timeout_ms: u64,
    },
}

impl ServiceReadinessV2 {
    pub fn endpoint_name(&self) -> &str {
        match self {
            Self::Http { endpoint_name, .. } | Self::Tcp { endpoint_name, .. } => endpoint_name,
        }
    }

    pub fn timeout_ms(&self) -> u64 {
        match self {
            Self::Http { timeout_ms, .. } | Self::Tcp { timeout_ms, .. } => *timeout_ms,
        }
    }
}

impl OciServiceGroupV2 {
    /// The one Surface Endpoint of the group and the service that serves it.
    pub fn surface(&self) -> Option<(&OciServiceV2, &ServiceEndpointV2)> {
        self.services.iter().find_map(|service| {
            service
                .endpoints
                .iter()
                .find(|endpoint| endpoint.exposure == EndpointExposureV2::Surface)
                .map(|endpoint| (service, endpoint))
        })
    }
}

impl RuntimeLaunchSpecV2 {
    pub fn service_group(&self) -> &OciServiceGroupV2 {
        match &self.realization {
            LaunchRealizationV2::OciServiceGroup(group) => group,
        }
    }

    /// Every invariant an executor is entitled to assume. Run at both ends.
    pub fn validate(&self) -> Result<(), RuntimeLaunchSpecError> {
        if self.protocol != RUNTIME_LAUNCH_SPEC_V2_PROTOCOL {
            return Err(RuntimeLaunchSpecError::UnsupportedVersion {
                found: self.protocol.clone(),
            });
        }
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
        if !self.workspace.cwd_relative.is_empty() {
            return Err(RuntimeLaunchSpecError::InvalidCwd {
                cwd: self.workspace.cwd_relative.clone(),
            });
        }

        let group = self.service_group();
        if !matches!(group.platform.as_str(), "linux/amd64" | "linux/arm64") {
            return Err(RuntimeLaunchSpecError::InvalidImageDigest {
                reference: group.platform.clone(),
            });
        }
        if !(OCI_SERVICE_GROUP_MIN_SERVICES..=OCI_SERVICE_GROUP_MAX_SERVICES)
            .contains(&group.services.len())
        {
            return Err(group_error("realization.services"));
        }

        let mut state_by_key = BTreeMap::new();
        for attachment in &self.state_attachments {
            validate_mount_target(&attachment.mount_target)?;
            if attachment.state_key.is_empty()
                || state_by_key
                    .insert(attachment.state_key.as_str(), attachment)
                    .is_some()
            {
                return Err(RuntimeLaunchSpecError::StateKeyConflict {
                    key: attachment.state_key.clone(),
                });
            }
        }

        let mut names = BTreeSet::new();
        // Secrets are redeemed once per Run and handed out by name, so a name
        // must identify one grant across the whole group. Bindings are scoped
        // to one service each, which already makes this true by construction.
        let mut secret_names = BTreeSet::new();
        let mut endpoint_names = BTreeSet::new();
        let mut state_readers = BTreeMap::<&str, usize>::new();
        let mut surfaces = 0usize;
        let mut endpoint_count = 0usize;
        for service in &group.services {
            let field = |suffix: &str| format!("realization.services[{}].{suffix}", service.name);
            if !is_service_name(&service.name) || !names.insert(service.name.as_str()) {
                return Err(group_error(format!(
                    "realization.services[{}].name",
                    service.name
                )));
            }
            if !is_content_addressed_digest(&service.image_digest_ref) {
                return Err(RuntimeLaunchSpecError::InvalidImageDigest {
                    reference: service.image_digest_ref.clone(),
                });
            }
            let expected = format!("@{}", service.image_digest_ref);
            if !service.image_reference.ends_with(&expected)
                || service.image_reference.starts_with('@')
            {
                return Err(RuntimeLaunchSpecError::InvalidImageDigest {
                    reference: service.image_reference.clone(),
                });
            }
            if service.argv.is_empty()
                || service.argv[0].is_empty()
                || service.argv.iter().any(|value| value.contains('\0'))
            {
                return Err(RuntimeLaunchSpecError::EmptyArgv);
            }
            if service
                .entrypoint
                .as_deref()
                .is_some_and(|entrypoint| !is_guest_path(entrypoint))
            {
                return Err(RuntimeLaunchSpecError::ForbiddenField {
                    field: field("entrypoint"),
                });
            }
            for (name, value) in [
                ("working_dir", &service.working_dir),
                ("workspace_mount_path", &service.workspace_mount_path),
            ] {
                if !is_guest_path(value) {
                    return Err(RuntimeLaunchSpecError::ForbiddenField { field: field(name) });
                }
            }

            // One environment namespace per container: siblings may reuse a
            // name, but within a service a secret must never be shadowed by a
            // public value or the other way round.
            let mut env_names = BTreeSet::new();
            for env in &service.public_env {
                if env.name.is_empty()
                    || env.name.contains(['=', '\0'])
                    || env.value.contains('\0')
                    || !env_names.insert(env.name.as_str())
                {
                    return Err(RuntimeLaunchSpecError::EnvConflict {
                        name: env.name.clone(),
                    });
                }
            }
            for grant in &service.secret_grants {
                if grant.name.is_empty()
                    || grant.name.contains(['=', '\0'])
                    || !env_names.insert(grant.name.as_str())
                    || !secret_names.insert(grant.name.as_str())
                {
                    return Err(RuntimeLaunchSpecError::EnvConflict {
                        name: grant.name.clone(),
                    });
                }
                if grant.grant_ref.is_empty() {
                    return Err(RuntimeLaunchSpecError::ForbiddenField {
                        field: field(&format!("secret_grants[{}].grant_ref", grant.name)),
                    });
                }
            }

            let mut targets = BTreeSet::from([service.workspace_mount_path.as_str()]);
            let mut seen = BTreeSet::new();
            for key in &service.state_keys {
                let Some(attachment) = state_by_key.get(key.as_str()) else {
                    return Err(RuntimeLaunchSpecError::StateKeyConflict { key: key.clone() });
                };
                if !seen.insert(key.as_str()) {
                    return Err(RuntimeLaunchSpecError::StateKeyConflict { key: key.clone() });
                }
                if !targets.insert(attachment.mount_target.as_str()) {
                    return Err(RuntimeLaunchSpecError::MountConflict {
                        target: attachment.mount_target.clone(),
                    });
                }
                *state_readers.entry(key.as_str()).or_default() += 1;
            }

            if service.endpoints.is_empty() {
                return Err(group_error(field("endpoints")));
            }
            let mut guest_ports = BTreeSet::new();
            for endpoint in &service.endpoints {
                endpoint_count += 1;
                if endpoint.name.is_empty() || !endpoint_names.insert(endpoint.name.as_str()) {
                    return Err(RuntimeLaunchSpecError::EndpointConflict {
                        name: endpoint.name.clone(),
                    });
                }
                let expected_protocol = match endpoint.exposure {
                    EndpointExposureV2::Surface => {
                        surfaces += 1;
                        SERVICE_ENDPOINT_HTTP
                    }
                    EndpointExposureV2::Internal => SERVICE_ENDPOINT_TCP,
                };
                if endpoint.guest_port == 0
                    || !guest_ports.insert(endpoint.guest_port)
                    || endpoint.protocol != expected_protocol
                {
                    return Err(RuntimeLaunchSpecError::InvalidEndpoint {
                        name: endpoint.name.clone(),
                    });
                }
            }
            let readiness_endpoint = service
                .endpoints
                .iter()
                .find(|endpoint| endpoint.name == service.readiness.endpoint_name());
            let readiness_matches = match (&service.readiness, readiness_endpoint) {
                (ServiceReadinessV2::Http { path, .. }, Some(endpoint)) => {
                    endpoint.protocol == SERVICE_ENDPOINT_HTTP && path.starts_with('/')
                }
                (ServiceReadinessV2::Tcp { .. }, Some(_)) => true,
                (_, None) => false,
            };
            if !readiness_matches {
                return Err(RuntimeLaunchSpecError::InvalidReadiness {
                    endpoint: service.readiness.endpoint_name().to_owned(),
                });
            }
            if service.readiness.timeout_ms() == 0 {
                return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                    field: field("readiness.timeout_ms"),
                });
            }
        }
        if surfaces != 1 || endpoint_count > OCI_SERVICE_GROUP_MAX_PORTS {
            return Err(group_error("realization.services[].endpoints"));
        }
        if let Some(key) = state_by_key
            .keys()
            .find(|key| state_readers.get(*key) != Some(&1))
        {
            return Err(RuntimeLaunchSpecError::StateKeyConflict {
                key: (*key).to_owned(),
            });
        }

        let totals = group_totals(group.services.iter().map(|service| {
            (
                service.resource_limits.memory_bytes,
                service.resource_limits.cpu_limit_millis,
                service.resource_limits.pids_limit,
            )
        }))
        .ok_or_else(|| group_error("realization.services[].resource_limits"))?;
        let stated = &group.total_limits;
        if totals
            != (
                stated.memory_bytes,
                stated.cpu_limit_millis,
                stated.pids_limit,
            )
        {
            return Err(group_error("realization.total_limits"));
        }

        if self.lifecycle.graceful_shutdown_ms == 0 {
            return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                field: "lifecycle.graceful_shutdown_ms".to_owned(),
            });
        }
        if self.lifecycle.force_kill_after_ms <= self.lifecycle.graceful_shutdown_ms {
            return Err(RuntimeLaunchSpecError::InvalidLifecycle {
                field: "lifecycle.force_kill_after_ms".to_owned(),
            });
        }
        Ok(())
    }

    /// RFC 8785 canonical bytes of a valid spec.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RuntimeLaunchSpecError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|_| RuntimeLaunchSpecError::ForbiddenField {
            field: "canonicalization".to_owned(),
        })
    }

    pub fn canonical_digest(&self) -> Result<String, RuntimeLaunchSpecError> {
        use sha2::{Digest, Sha256};
        let bytes = self.canonical_bytes()?;
        Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
    }

    pub fn parse(raw: &str) -> Result<Self, RuntimeLaunchSpecError> {
        let spec: Self =
            serde_json::from_str(raw).map_err(|error| RuntimeLaunchSpecError::ForbiddenField {
                field: format!("payload: {error}"),
            })?;
        spec.validate()?;
        Ok(spec)
    }
}

/// A launch spec of any version this build understands, chosen by its
/// `protocol` BEFORE the body is interpreted. An unknown protocol is refused
/// rather than parsed as the nearest known shape.
// One value per Run, parsed once; boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLaunchSpec {
    V1(RuntimeLaunchSpecV1),
    V2(RuntimeLaunchSpecV2),
    /// A v3 spec with its v2 group view: everything that handles a group
    /// reads `view`; only state backing reads `spec`.
    V3 {
        spec: RuntimeLaunchSpecV3,
        view: RuntimeLaunchSpecV2,
    },
}

impl RuntimeLaunchSpec {
    pub fn parse(raw: &str) -> Result<Self, RuntimeLaunchSpecError> {
        #[derive(Deserialize)]
        struct Discriminator {
            protocol: String,
        }
        let discriminator: Discriminator =
            serde_json::from_str(raw).map_err(|error| RuntimeLaunchSpecError::ForbiddenField {
                field: format!("payload: {error}"),
            })?;
        match discriminator.protocol.as_str() {
            RUNTIME_LAUNCH_SPEC_V1_PROTOCOL => RuntimeLaunchSpecV1::parse(raw).map(Self::V1),
            RUNTIME_LAUNCH_SPEC_V2_PROTOCOL => RuntimeLaunchSpecV2::parse(raw).map(Self::V2),
            RUNTIME_LAUNCH_SPEC_V3_PROTOCOL => RuntimeLaunchSpecV3::parse(raw).map(|spec| {
                let view = spec.group_view();
                Self::V3 { spec, view }
            }),
            other => Err(RuntimeLaunchSpecError::UnsupportedVersion {
                found: other.to_owned(),
            }),
        }
    }

    pub fn canonical_digest(&self) -> Result<String, RuntimeLaunchSpecError> {
        match self {
            Self::V1(spec) => spec.canonical_digest(),
            Self::V2(spec) => spec.canonical_digest(),
            // The digest is always of the bytes that were sent, never of the
            // derived view.
            Self::V3 { spec, .. } => spec.canonical_digest(),
        }
    }

    pub fn context(&self) -> &LaunchContextV1 {
        match self {
            Self::V1(spec) => &spec.context,
            Self::V2(spec) | Self::V3 { view: spec, .. } => &spec.context,
        }
    }

    pub fn state_attachments(&self) -> &[StateAttachmentV1] {
        match self {
            Self::V1(spec) => &spec.state_attachments,
            Self::V2(spec) | Self::V3 { view: spec, .. } => &spec.state_attachments,
        }
    }

    pub fn lifecycle(&self) -> &LifecycleV1 {
        match self {
            Self::V1(spec) => &spec.lifecycle,
            Self::V2(spec) | Self::V3 { view: spec, .. } => &spec.lifecycle,
        }
    }

    /// The Run as an OCI service group, for either group protocol.
    pub fn service_group_spec(&self) -> Option<&RuntimeLaunchSpecV2> {
        match self {
            Self::V1(_) => None,
            Self::V2(spec) | Self::V3 { view: spec, .. } => Some(spec),
        }
    }

    /// The Runner-local volume behind a state key, when this is a v3 Run.
    /// `None` means the key is revision-backed (or absent).
    pub fn runner_volume(&self, state_key: &str) -> Option<&RunnerVolumeBackingV3> {
        match self {
            Self::V3 { spec, .. } => spec
                .state_attachments
                .iter()
                .find(|attachment| attachment.state_key == state_key)
                .map(StateAttachmentV3::runner_volume),
            Self::V1(_) | Self::V2(_) => None,
        }
    }

    /// Every secret the Run needs redeemed, across all services.
    pub fn secret_grants(&self) -> Vec<SecretGrantV1> {
        match self {
            Self::V1(spec) => spec.secret_grants.clone(),
            Self::V2(spec) | Self::V3 { view: spec, .. } => spec
                .service_group()
                .services
                .iter()
                .flat_map(|service| service.secret_grants.iter().cloned())
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests;
