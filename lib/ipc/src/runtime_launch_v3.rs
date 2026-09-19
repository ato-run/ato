//! Launch contract v3: an OCI service group whose state lives in a Runner-local
//! persistent volume.
//!
//! v3 changes exactly one thing relative to v2: what a state attachment IS. A
//! v2 attachment names a State Revision the Runner restores into a per-Run
//! working copy and packs back afterwards. A v3 attachment names a volume that
//! already lives on one Runner and is mounted as-is: its contents are the
//! authoritative state, and a State Revision is at most the seed it was first
//! created from.
//!
//! That is a different meaning for the same field name, so it is a new
//! protocol rather than an optional field on v2. A Runner that does not know
//! v3 refuses it by `protocol` before anything is materialized; it can never
//! read a volume attachment as a working-copy one. The v1 and v2 types, bytes
//! and digests are untouched.
//!
//! The spec carries a `volume_ref`, never a host path: where the volume lives
//! on the Runner is the Runner's business, and nothing here may name it.

use serde::{Deserialize, Serialize};

use crate::runtime_launch::{
    LaunchContextV1, LaunchWorkspaceV1, LifecycleV1, RuntimeLaunchSpecError, StateAccessV1,
    StateAttachmentV1,
};
use crate::runtime_launch_v2::{
    LaunchRealizationV2, OciServiceGroupV2, RUNTIME_LAUNCH_SPEC_V2_PROTOCOL, RuntimeLaunchSpecV2,
};

pub const RUNTIME_LAUNCH_SPEC_V3_PROTOCOL: &str = "ato.runtime-launch-spec.v3";

/// `svol_` + a Crockford ULID. Opaque: a storage identity, not a digest.
const VOLUME_REF_PREFIX: &str = "svol_";
const ULID_LEN: usize = 26;
const MAX_REVISION_REF_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLaunchSpecV3 {
    pub protocol: String,
    pub context: LaunchContextV1,
    /// `cwd_relative` must be empty: each service names its own working dir.
    pub workspace: LaunchWorkspaceV1,
    pub realization: LaunchRealizationV2,
    /// Slots of this Run. Each is mounted into exactly one service, named by
    /// that service's `state_keys`.
    pub state_attachments: Vec<StateAttachmentV3>,
    pub lifecycle: LifecycleV1,
}

/// A state attachment backed by a Runner-local persistent volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateAttachmentV3 {
    pub state_key: String,
    /// Absolute path INSIDE the guest, e.g. `/data`.
    pub mount_target: String,
    /// Always `read_write`: a volume attachment is a writer.
    pub access: StateAccessV1,
    /// The writer generation. Required here — a volume is written in place,
    /// so there is no attachment without a writer.
    pub writer_fence: u64,
    pub backing: StateBackingV3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateBackingV3 {
    RunnerVolume(RunnerVolumeBackingV3),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerVolumeBackingV3 {
    pub volume_ref: String,
    /// The capacity the control plane admitted this volume for. The Runner
    /// refuses to start when it cannot offer it.
    pub capacity_bytes: u64,
    /// Seed for a volume that does not exist yet, unpacked exactly once.
    /// Ignored for a volume that does: an existing volume is newer than any
    /// revision, and re-seeding it would silently roll data back.
    ///
    /// Always serialized, including when null, for the same reason as
    /// `StateAttachmentV1::revision_ref`: Rust and TypeScript must
    /// canonicalize the same spec into the same bytes.
    #[serde(default)]
    pub initialize_from_revision_ref: Option<String>,
}

impl StateAttachmentV3 {
    pub fn runner_volume(&self) -> &RunnerVolumeBackingV3 {
        match &self.backing {
            StateBackingV3::RunnerVolume(volume) => volume,
        }
    }
}

/// True for `svol_<ULID>` and nothing else — the only shape a Runner will
/// turn into a directory name.
pub fn is_volume_ref(value: &str) -> bool {
    value.strip_prefix(VOLUME_REF_PREFIX).is_some_and(|ulid| {
        ulid.len() == ULID_LEN
            && ulid.bytes().all(|byte| {
                byte.is_ascii_digit()
                    || (byte.is_ascii_uppercase() && !matches!(byte, b'I' | b'L' | b'O' | b'U'))
            })
    })
}

fn is_revision_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REVISION_REF_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
}

fn volume_error(field: impl Into<String>) -> RuntimeLaunchSpecError {
    RuntimeLaunchSpecError::InvalidStateVolume {
        field: field.into(),
    }
}

impl RuntimeLaunchSpecV3 {
    pub fn service_group(&self) -> &OciServiceGroupV2 {
        match &self.realization {
            LaunchRealizationV2::OciServiceGroup(group) => group,
        }
    }

    /// The same Run seen through the v2 group rules. Every group invariant is
    /// checked once, in v2; v3 adds only the volume rules on top.
    ///
    /// The view's attachments carry no `revision_ref` — a volume is not
    /// restored from one — and the v3 writer fence.
    pub fn group_view(&self) -> RuntimeLaunchSpecV2 {
        RuntimeLaunchSpecV2 {
            protocol: RUNTIME_LAUNCH_SPEC_V2_PROTOCOL.to_owned(),
            context: self.context.clone(),
            workspace: self.workspace.clone(),
            realization: self.realization.clone(),
            state_attachments: self
                .state_attachments
                .iter()
                .map(|attachment| StateAttachmentV1 {
                    state_key: attachment.state_key.clone(),
                    revision_ref: None,
                    mount_target: attachment.mount_target.clone(),
                    access: attachment.access,
                    writer_fence: Some(attachment.writer_fence),
                })
                .collect(),
            lifecycle: self.lifecycle.clone(),
        }
    }

    /// Every invariant an executor is entitled to assume. Run at both ends.
    pub fn validate(&self) -> Result<(), RuntimeLaunchSpecError> {
        if self.protocol != RUNTIME_LAUNCH_SPEC_V3_PROTOCOL {
            return Err(RuntimeLaunchSpecError::UnsupportedVersion {
                found: self.protocol.clone(),
            });
        }
        self.group_view().validate()?;
        if self.state_attachments.is_empty() {
            // v3 exists to carry a volume. Without one it would be a v2 spec
            // under another name, and two names for one meaning is how the
            // versions drift.
            return Err(volume_error("state_attachments"));
        }
        let mut volumes = std::collections::BTreeSet::new();
        for attachment in &self.state_attachments {
            let field =
                |suffix: &str| format!("state_attachments[{}].{suffix}", attachment.state_key);
            if attachment.access != StateAccessV1::ReadWrite {
                return Err(volume_error(field("access")));
            }
            let volume = attachment.runner_volume();
            if !is_volume_ref(&volume.volume_ref) || !volumes.insert(volume.volume_ref.as_str()) {
                return Err(volume_error(field("backing.volume_ref")));
            }
            if volume.capacity_bytes == 0 {
                return Err(volume_error(field("backing.capacity_bytes")));
            }
            if volume
                .initialize_from_revision_ref
                .as_deref()
                .is_some_and(|revision| !is_revision_ref(revision))
            {
                return Err(volume_error(field("backing.initialize_from_revision_ref")));
            }
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

#[cfg(test)]
mod tests;
