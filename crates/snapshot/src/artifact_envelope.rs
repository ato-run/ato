//! Artifact Envelope — packaging a Capsule v1 [`SnapshotManifestV1`] and its
//! compatibility contract, together with the legacy transport artifact it was
//! captured alongside, into one authenticated unit for transport (R2 upload /
//! runner-side restore lease) and local on-disk publication.
//!
//! [`capsule::snapshot_manifest`] deliberately stops at the pure identity +
//! compatibility contract: the `cas_root_digest` and acceptance metadata are
//! explicitly out of scope there (the `capsule` crate cannot depend on
//! `capsulefs`/`snapshot`). This module is the `snapshot`-crate composition
//! point that closes that gap:
//!
//! * [`ArtifactEnvelopeV1`] (schema [`ARTIFACT_ENVELOPE_V1_SCHEMA`]) binds
//!   together the legacy [`ReadyStateManifest`]'s content (via
//!   `cas_root_digest`, a domain-separated hash of its layer refs), the v1
//!   [`SnapshotManifestV1`]'s identity + compatibility (`snapshot_manifest_id`
//!   / `compatibility`), and an [`ArtifactAcceptance`] disposition + receipt
//!   id — everything a runner-side restore lease needs to trust a downloaded
//!   artifact bundle without re-deriving it from scratch.
//! * [`ArtifactEnvelopeV1::envelope_id`] is a content address over every other
//!   field (domain `ato.snapshot-artifact-envelope/v1`), so a tampered
//!   envelope (e.g. a locally "promoted" `Quarantined` → `Accepted` status)
//!   fails [`ArtifactEnvelopeV1::verify`] closed — acceptance state can never
//!   be silently upgraded after the fact.
//! * [`ArtifactEnvelopeV1::accepted`] is the sanctioned constructor: it only
//!   ever mints an envelope in the [`ArtifactAcceptanceStatus::Accepted`]
//!   state, over an already-[`SnapshotManifestV1::validate`]-passing
//!   candidate. There is no public way to construct a `Quarantined` envelope —
//!   quarantine is expressed by *not* re-deriving one, and is caller
//!   (catalog/registry) state, never a value this module hands out.
use capsule::execution_contract::{ContentDigest, DigestAlgorithm};
use capsule::snapshot_manifest::{
    SNAPSHOT_MANIFEST_V1_SCHEMA, SnapshotCompatibilityContractV1, SnapshotId, SnapshotManifestV1,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::ReadyStateManifest;

/// Schema tag for the Artifact Envelope wire format.
pub const ARTIFACT_ENVELOPE_V1_SCHEMA: &str = "ato.snapshot-artifact-envelope/v1";
/// Sidecar filename this envelope is written under next to `manifest.json` /
/// `snapshot-manifest-v1.json` / `cas/` in a transport artifact bundle.
pub const ARTIFACT_ENVELOPE_V1_FILENAME: &str = "artifact-envelope-v1.json";
/// Sidecar filename the Capsule v1 [`SnapshotManifestV1`] itself is written
/// under, next to `manifest.json` / [`ARTIFACT_ENVELOPE_V1_FILENAME`] / `cas/`
/// in a transport artifact bundle. Lives here (rather than on
/// `capsule::snapshot_manifest`, which is transport-agnostic) because a
/// filename is a `snapshot`-crate packaging concern.
pub const SNAPSHOT_MANIFEST_V1_FILENAME: &str = "snapshot-manifest-v1.json";
const ARTIFACT_ENVELOPE_ID_DOMAIN: &[u8] = b"ato.snapshot-artifact-envelope/v1\0";
const CAS_ROOT_ID_DOMAIN: &[u8] = b"ato.snapshot-cas-root/v1\0";
const ACCEPTANCE_RECEIPT_ID_DOMAIN: &[u8] = b"ato.snapshot-acceptance-receipt/v1\0";

/// The `ato.snapshot-artifact-envelope/v1` wire contract: an authenticated
/// binding between a legacy transport artifact's content, a Capsule v1
/// [`SnapshotManifestV1`]'s identity + compatibility, and an acceptance
/// disposition. See the module docs for the full rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEnvelopeV1 {
    /// Always [`ARTIFACT_ENVELOPE_V1_SCHEMA`]; validated on read.
    pub schema: String,
    /// Content address of every other field (domain-separated); recomputed and
    /// checked by [`Self::verify`].
    pub envelope_id: String,
    /// The legacy [`ReadyStateManifest::id`] this envelope authenticates.
    pub legacy_manifest_id: String,
    /// The Capsule v1 Snapshot manifest schema tag (always
    /// [`SNAPSHOT_MANIFEST_V1_SCHEMA`]; carried explicitly so the envelope is
    /// self-describing without re-parsing the manifest).
    pub snapshot_manifest_schema: String,
    /// The Capsule v1 [`SnapshotManifestV1::snapshot_id`] this envelope
    /// authenticates.
    pub snapshot_manifest_id: SnapshotId,
    /// The Snapshot's compatibility contract, mirrored here so a runner-side
    /// restore lease can inspect compatibility without trusting an
    /// unauthenticated copy of the manifest.
    pub compatibility: SnapshotCompatibilityContractV1,
    /// Domain-separated content address of the legacy manifest's CapsuleFS
    /// layer refs — the "root of trust" a restore lease verifies its
    /// downloaded `cas/` tree against.
    pub cas_root_digest: ContentDigest,
    /// Acceptance disposition + receipt id.
    pub acceptance: ArtifactAcceptance,
}

/// Acceptance metadata carried inside the envelope. Unlike
/// [`capsule::snapshot_manifest::AcceptanceStatus`] (a 3-state *catalog*
/// disposition: accepted / rejected / quarantined), an envelope is only ever
/// minted for an already-accepted candidate — there is no "rejected envelope"
/// — so this is deliberately a narrower 2-state type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAcceptance {
    pub status: ArtifactAcceptanceStatus,
    /// Content address identifying the verifier + disposition that produced
    /// this acceptance (see [`acceptance_receipt_id`]).
    pub receipt_id: ContentDigest,
}

/// The envelope's acceptance disposition. Quarantine is expressed by the
/// *caller* (a local catalog / registry) refusing to trust an envelope it
/// otherwise holds — never by this module minting one in this state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAcceptanceStatus {
    Accepted,
    Quarantined,
}

/// The exact byte projection [`ArtifactEnvelopeV1::envelope_id`] is derived
/// from — every field of [`ArtifactEnvelopeV1`] except `envelope_id` itself,
/// so the id is never part of its own preimage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EnvelopeIdentityProjection<'a> {
    schema: &'a str,
    legacy_manifest_id: &'a str,
    snapshot_manifest_schema: &'a str,
    snapshot_manifest_id: &'a SnapshotId,
    compatibility: &'a SnapshotCompatibilityContractV1,
    cas_root_digest: ContentDigest,
    acceptance: &'a ArtifactAcceptance,
}

/// The exact byte projection [`ArtifactAcceptance::receipt_id`] is derived
/// from: the accepted Snapshot's id, the disposition, and the verifier
/// identity that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AcceptanceReceiptProjection<'a> {
    snapshot_id: &'a SnapshotId,
    status: ArtifactAcceptanceStatus,
    verifier: &'a str,
}

/// Errors constructing or verifying an [`ArtifactEnvelopeV1`]. Every variant
/// means the envelope must not be trusted: [`ArtifactEnvelopeV1::verify`] fails
/// closed rather than returning a partial/best-effort match.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArtifactEnvelopeError {
    #[error("artifact envelope schema must be {ARTIFACT_ENVELOPE_V1_SCHEMA}")]
    InvalidSchema,
    #[error("artifact envelope carries an invalid content-addressed id")]
    InvalidId,
    #[error("artifact envelope does not authenticate the supplied legacy manifest")]
    LegacyManifestMismatch,
    #[error("artifact envelope does not authenticate the supplied Snapshot manifest")]
    SnapshotManifestMismatch,
    #[error("artifact envelope compatibility evidence differs from the Snapshot manifest")]
    CompatibilityMismatch,
    #[error("artifact envelope is not in accepted state")]
    NotAccepted,
    #[error("failed to canonicalize artifact envelope: {0}")]
    Canonicalization(String),
}

impl ArtifactEnvelopeV1 {
    /// Mint an authenticated, [`ArtifactAcceptanceStatus::Accepted`] envelope
    /// binding `legacy` (the transport artifact whose CapsuleFS layers this
    /// Snapshot restores) to `snapshot` (the Capsule v1 identity +
    /// compatibility sidecar for those same immutable layers). Fails closed if
    /// `snapshot` does not itself pass [`SnapshotManifestV1::validate`].
    pub fn accepted(
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
    ) -> Result<Self, ArtifactEnvelopeError> {
        snapshot
            .validate()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        let snapshot_manifest_id = snapshot
            .snapshot_id()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        let cas_root_digest = cas_root_digest(legacy)?;
        let acceptance = ArtifactAcceptance {
            status: ArtifactAcceptanceStatus::Accepted,
            receipt_id: acceptance_receipt_id(&snapshot_manifest_id)?,
        };
        let mut envelope = Self {
            schema: ARTIFACT_ENVELOPE_V1_SCHEMA.to_string(),
            envelope_id: String::new(),
            legacy_manifest_id: legacy.id(),
            snapshot_manifest_schema: snapshot.schema.clone(),
            snapshot_manifest_id,
            compatibility: snapshot.compatibility_contract.clone(),
            cas_root_digest,
            acceptance,
        };
        envelope.envelope_id = envelope.compute_envelope_id()?;
        envelope.verify(legacy, snapshot)?;
        Ok(envelope)
    }

    /// Derive `envelope_id`: the domain-separated content address of every
    /// other field (see [`EnvelopeIdentityProjection`]).
    pub fn compute_envelope_id(&self) -> Result<String, ArtifactEnvelopeError> {
        let projection = EnvelopeIdentityProjection {
            schema: &self.schema,
            legacy_manifest_id: &self.legacy_manifest_id,
            snapshot_manifest_schema: &self.snapshot_manifest_schema,
            snapshot_manifest_id: &self.snapshot_manifest_id,
            compatibility: &self.compatibility,
            cas_root_digest: self.cas_root_digest,
            acceptance: &self.acceptance,
        };
        domain_hash(ARTIFACT_ENVELOPE_ID_DOMAIN, &projection).map(|digest| digest.to_string())
    }

    /// Fail-closed consumer boundary: re-derive `envelope_id` and re-check
    /// every authenticated binding against the supplied `legacy` /`snapshot`
    /// pair. A caller (restore lease, local publication reader) MUST call this
    /// before trusting an envelope it did not itself just mint — a
    /// self-consistent-looking envelope whose bindings were hand-edited
    /// (tampered sidecar, locally "promoted" acceptance) is rejected here.
    pub fn verify(
        &self,
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
    ) -> Result<(), ArtifactEnvelopeError> {
        if self.schema != ARTIFACT_ENVELOPE_V1_SCHEMA {
            return Err(ArtifactEnvelopeError::InvalidSchema);
        }
        if self.envelope_id != self.compute_envelope_id()? {
            return Err(ArtifactEnvelopeError::InvalidId);
        }
        if self.legacy_manifest_id != legacy.id()
            || self.cas_root_digest != cas_root_digest(legacy)?
        {
            return Err(ArtifactEnvelopeError::LegacyManifestMismatch);
        }
        snapshot
            .validate()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        let snapshot_manifest_id = snapshot
            .snapshot_id()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        if self.snapshot_manifest_schema != SNAPSHOT_MANIFEST_V1_SCHEMA
            || self.snapshot_manifest_schema != snapshot.schema
            || self.snapshot_manifest_id != snapshot_manifest_id
        {
            return Err(ArtifactEnvelopeError::SnapshotManifestMismatch);
        }
        if self.compatibility != snapshot.compatibility_contract {
            return Err(ArtifactEnvelopeError::CompatibilityMismatch);
        }
        if self.acceptance.status != ArtifactAcceptanceStatus::Accepted
            || self.acceptance.receipt_id != acceptance_receipt_id(&snapshot_manifest_id)?
        {
            return Err(ArtifactEnvelopeError::NotAccepted);
        }
        Ok(())
    }
}

/// `cas_root_digest`: a domain-separated content address of the legacy
/// manifest's CapsuleFS layer refs (the transport artifact's `cas/` root of
/// trust).
fn cas_root_digest(legacy: &ReadyStateManifest) -> Result<ContentDigest, ArtifactEnvelopeError> {
    domain_hash(CAS_ROOT_ID_DOMAIN, &legacy.layers)
}

/// `acceptance.receipt_id`: a domain-separated content address of the accepted
/// Snapshot id, disposition, and verifier identity. Pinned to
/// `"platform-disposable-restore/v1"` — the only verifier this crate mints an
/// envelope on behalf of today (see `snapshot::acceptance`).
fn acceptance_receipt_id(snapshot_id: &SnapshotId) -> Result<ContentDigest, ArtifactEnvelopeError> {
    domain_hash(
        ACCEPTANCE_RECEIPT_ID_DOMAIN,
        &AcceptanceReceiptProjection {
            snapshot_id,
            status: ArtifactAcceptanceStatus::Accepted,
            verifier: "platform-disposable-restore/v1",
        },
    )
}

/// Shared domain-separated hash helper: `BLAKE3(domain || JCS(value))`.
fn domain_hash(
    domain: &[u8],
    value: &impl Serialize,
) -> Result<ContentDigest, ArtifactEnvelopeError> {
    let canonical = serde_jcs::to_vec(value)
        .map_err(|error| ArtifactEnvelopeError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&canonical);
    Ok(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::ExecutionId;
    use capsule::snapshot_manifest::{
        CapturePolicyV1, PortabilityTier, RestoreContractV1, SNAPSHOT_COMPATIBILITY_V1_SCHEMA,
        SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA, SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA,
        SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA, SanitizationAttestationV1,
        SecretScanAttestationV1, SnapshotBackendKind, SnapshotCaptureProvenance,
    };
    use capsulefs::{CasStore, ChunkingKind, HotsetProfile, LayerKind, store_blob};

    use super::*;
    use crate::manifest::{
        ReadyStateLayers, RestoreContract, SanitizerContract, SnapshotBackendInfo,
    };

    fn digest(fill: char) -> ContentDigest {
        assert!(fill.is_ascii_hexdigit() && !fill.is_ascii_uppercase());
        ContentDigest::try_from(format!("blake3:{}", fill.to_string().repeat(64)))
            .expect("valid content digest")
    }

    fn execution_id(fill: char) -> ExecutionId {
        ExecutionId::new(format!("blake3:{}", fill.to_string().repeat(64))).expect("valid id")
    }

    fn compatibility(format_version: u32) -> SnapshotCompatibilityContractV1 {
        SnapshotCompatibilityContractV1 {
            schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
            backend: SnapshotBackendKind::Firecracker,
            format_version,
            vmm_identity: "firecracker-1.7".to_string(),
            state_codec: "fc-state/v2".to_string(),
            guest_kernel_identity: "vmlinux-6.1-ato".to_string(),
            cpu_template: "T2CL".to_string(),
            runner_restore_contract: "ato-restore/v1".to_string(),
            portability_tier: PortabilityTier::ClassPortable,
            compatibility_class_identity: digest('c'),
        }
    }

    fn snapshot_manifest(execution: ExecutionId, format_version: u32) -> SnapshotManifestV1 {
        SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: execution,
            compatibility_contract: compatibility(format_version),
            memory_layer_refs: vec![digest('1')],
            vmstate_layer_refs: vec![digest('2')],
            disk_layer_refs: vec![digest('3')],
            restore_contract: RestoreContractV1 {
                schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
                restore_protocol: "ato-restore/v1".to_string(),
                steps: vec!["network_reconnect".to_string()],
            },
            capture_policy: CapturePolicyV1::Running,
            capture_provenance: SnapshotCaptureProvenance::default(),
            sanitization_attestation: SanitizationAttestationV1 {
                schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
                steps: vec!["session_id_regenerate".to_string()],
            },
            secret_scan_attestation: SecretScanAttestationV1 {
                schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
                scanner_identity: "ato-secret-scan/1.0".to_string(),
                policy_identity: "default/v1".to_string(),
                scanned_layers: vec!["memory".to_string(), "vmstate".to_string()],
                verdict: "clean".to_string(),
            },
        }
    }

    fn legacy_manifest() -> (tempfile::TempDir, ReadyStateManifest) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let rootfs = store_blob(
            &store,
            LayerKind::Rootfs,
            b"rootfs-bytes",
            ChunkingKind::ContentDefined,
        )
        .unwrap();
        let legacy = ReadyStateManifest {
            schema: crate::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: format!("blake3:{}", "4".repeat(64)),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            execution_identity_schema: None,
            surface_requirement: None,
            layers: ReadyStateLayers {
                rootfs: Some(rootfs),
                ..ReadyStateLayers::default()
            },
            hotset_profile: HotsetProfile::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: "firecracker".to_string(),
                version: "1.10".to_string(),
                snapshot_format_version: "fc-v1".to_string(),
                cpu_template: Some("T2CL".to_string()),
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        (dir, legacy)
    }

    #[test]
    fn accepted_envelope_verifies() {
        let (_dir, legacy) = legacy_manifest();
        let snapshot = snapshot_manifest(execution_id('a'), 2);
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        assert_eq!(
            envelope.acceptance.status,
            ArtifactAcceptanceStatus::Accepted
        );
        envelope.verify(&legacy, &snapshot).unwrap();
    }

    #[test]
    fn tampered_sidecar_is_rejected_by_the_envelope_boundary() {
        let (_dir, legacy) = legacy_manifest();
        let snapshot = snapshot_manifest(execution_id('a'), 2);
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        let mut tampered = snapshot;
        tampered
            .sanitization_attestation
            .steps
            .push("attacker".to_string());

        assert_eq!(
            envelope.verify(&legacy, &tampered),
            Err(ArtifactEnvelopeError::SnapshotManifestMismatch)
        );
    }

    #[test]
    fn acceptance_state_is_authenticated_and_cannot_be_promoted_locally() {
        let (_dir, legacy) = legacy_manifest();
        let snapshot = snapshot_manifest(execution_id('a'), 2);
        let mut envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        envelope.acceptance.status = ArtifactAcceptanceStatus::Quarantined;

        assert_eq!(
            envelope.verify(&legacy, &snapshot),
            Err(ArtifactEnvelopeError::InvalidId)
        );
    }

    #[test]
    fn tampered_legacy_manifest_is_rejected() {
        let (_dir, legacy) = legacy_manifest();
        let snapshot = snapshot_manifest(execution_id('b'), 2);
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        let mut tampered_legacy = legacy;
        tampered_legacy.capsule_manifest_hash = format!("blake3:{}", "9".repeat(64));

        assert_eq!(
            envelope.verify(&tampered_legacy, &snapshot),
            Err(ArtifactEnvelopeError::LegacyManifestMismatch)
        );
    }

    #[test]
    fn compatibility_drift_is_rejected() {
        let (_dir, legacy) = legacy_manifest();
        let snapshot = snapshot_manifest(execution_id('c'), 2);
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        let mut drifted = snapshot;
        drifted.compatibility_contract.format_version = 3;

        // A different compatibility_contract also changes snapshot_id, so this
        // manifests as a SnapshotManifestMismatch (the id the envelope pins no
        // longer matches) rather than reaching the compatibility check — both
        // are fail-closed rejections, which is what matters here.
        assert!(envelope.verify(&legacy, &drifted).is_err());
    }
}
