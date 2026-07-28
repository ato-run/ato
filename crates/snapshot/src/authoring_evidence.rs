//! Builder-owned Clean Replay, state classification, and Ready-State Seal
//! evidence contracts.
//!
//! The orchestration is adapter-driven so production uses an isolated guest
//! while tests can prove ordering without KVM. Receipts are created only after
//! the adapter authenticates the measured result.

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule::authoring_intent::{NormalizedProgramIntentEnvelopeV1, WorkspacePathV1};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CLEAN_REPLAY_RECEIPT_V1_SCHEMA: &str = "ato.clean-replay-receipt/v1";
pub const RESTORE_VERIFICATION_RECEIPT_V1_SCHEMA: &str = "ato.restore-verification-receipt/v1";
pub const READY_STATE_SEAL_RECEIPT_V1_SCHEMA: &str = "ato.ready-state-seal-receipt/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanReplayRequestV1 {
    pub authoring_session_id: String,
    pub capsule_revision_id: String,
    pub source_closure_id: String,
    pub previous_receipt_digest: String,
    #[serde(default)]
    pub source_overlays: Vec<SourceOverlayArtifactV1>,
    pub normalized_program_intent: NormalizedProgramIntentEnvelopeV1,
    pub resolution_lock_digest: String,
    #[serde(default)]
    pub allowed_cache_digests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOverlayArtifactV1 {
    pub path: WorkspacePathV1,
    pub content_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveIsolationPostureV1 {
    pub ephemeral_workspace: bool,
    pub host_filesystem_hidden: bool,
    pub host_environment_inherited: bool,
    pub host_credentials_inherited: bool,
    pub privileged: bool,
    pub network_observed: bool,
    pub workspace_provenance: String,
}

impl EffectiveIsolationPostureV1 {
    fn validate(&self) -> Result<(), AuthoringEvidenceError> {
        if !self.ephemeral_workspace
            || !self.host_filesystem_hidden
            || self.host_environment_inherited
            || self.host_credentials_inherited
            || self.privileged
            || !self.network_observed
            || self.workspace_provenance.trim().is_empty()
        {
            return Err(AuthoringEvidenceError::InsufficientIsolation);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessResultV1 {
    pub ready: bool,
    pub probe_digest: String,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderAuthenticationV1 {
    pub key_id: String,
    pub algorithm: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanReplayReceiptV1 {
    pub payload_jcs_base64: String,
    pub authentication: BuilderAuthenticationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanReplayReceiptPayloadV1 {
    pub schema: String,
    pub receipt_id: String,
    pub authoring_session_id: String,
    pub capsule_revision_id: String,
    pub source_closure_id: String,
    pub program_intent_digest: String,
    pub resolution_lock_digest: String,
    pub previous_receipt_digest: String,
    pub builder_identity: String,
    pub materialization_inputs_digest: String,
    pub execution_contract_digest: String,
    pub readiness: ReadinessResultV1,
    pub effective_isolation_posture: EffectiveIsolationPostureV1,
    pub state_diff_digest: String,
    pub started_at: String,
    pub completed_at: String,
    pub issued_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanReplayObservationV1 {
    pub receipt_id: String,
    pub builder_identity: String,
    pub materialization_inputs_digest: String,
    pub execution_contract_digest: String,
    pub readiness: ReadinessResultV1,
    pub isolation: EffectiveIsolationPostureV1,
    pub state_diff: Vec<StateDiffEntryV1>,
    pub started_at: String,
    pub completed_at: String,
    pub issued_at: String,
    pub expires_at: String,
}

pub trait CleanReplayAdapter {
    /// Execute from a builder-created fresh workspace. The request has no path
    /// or handle to the authoring workspace by construction.
    fn replay(
        &mut self,
        request: &CleanReplayRequestV1,
    ) -> Result<CleanReplayObservationV1, String>;

    /// Authenticate canonical receipt payload bytes with the builder identity.
    fn authenticate(&mut self, payload: &[u8]) -> Result<BuilderAuthenticationV1, String>;
}

pub fn execute_clean_replay(
    adapter: &mut impl CleanReplayAdapter,
    request: &CleanReplayRequestV1,
) -> Result<(CleanReplayReceiptV1, ClassifiedStateDiffV1), AuthoringEvidenceError> {
    validate_replay_request(request)?;
    let observation = adapter
        .replay(request)
        .map_err(AuthoringEvidenceError::Adapter)?;
    observation.isolation.validate()?;
    if !observation.readiness.ready {
        return Err(AuthoringEvidenceError::ReadinessFailed);
    }
    validate_digest(
        "execution_contract_digest",
        &observation.execution_contract_digest,
    )?;
    let classified = classify_state_diff(
        &observation.state_diff,
        &request.normalized_program_intent.intent.build_output_roots,
    )?;
    let state_diff_digest = canonical_digest(b"ato.classified-state-diff/v1", &classified)?;
    let payload = CleanReplayReceiptPayloadV1 {
        schema: CLEAN_REPLAY_RECEIPT_V1_SCHEMA.to_string(),
        receipt_id: observation.receipt_id,
        authoring_session_id: request.authoring_session_id.clone(),
        capsule_revision_id: request.capsule_revision_id.clone(),
        source_closure_id: request.source_closure_id.clone(),
        program_intent_digest: request.normalized_program_intent.digest.clone(),
        resolution_lock_digest: request.resolution_lock_digest.clone(),
        previous_receipt_digest: request.previous_receipt_digest.clone(),
        builder_identity: observation.builder_identity,
        materialization_inputs_digest: observation.materialization_inputs_digest,
        execution_contract_digest: observation.execution_contract_digest,
        readiness: observation.readiness,
        effective_isolation_posture: observation.isolation,
        state_diff_digest,
        started_at: observation.started_at,
        completed_at: observation.completed_at,
        issued_at: observation.issued_at,
        expires_at: observation.expires_at,
    };
    validate_receipt_chain(
        &payload.receipt_id,
        &payload.authoring_session_id,
        &payload.capsule_revision_id,
        &payload.source_closure_id,
        &payload.previous_receipt_digest,
        &payload.issued_at,
        &payload.expires_at,
    )?;
    let canonical = canonical_payload(&payload)?;
    let authentication = adapter
        .authenticate(&canonical)
        .map_err(AuthoringEvidenceError::Adapter)?;
    let receipt = CleanReplayReceiptV1 {
        payload_jcs_base64: BASE64.encode(canonical),
        authentication,
    };
    Ok((receipt, classified))
}

impl CleanReplayReceiptV1 {
    pub fn payload(&self) -> Result<CleanReplayReceiptPayloadV1, AuthoringEvidenceError> {
        decode_payload(&self.payload_jcs_base64)
    }

    pub fn payload_digest(&self) -> Result<String, AuthoringEvidenceError> {
        signed_payload_digest(
            b"ato.clean-replay-receipt-reference/v1",
            &self.payload_jcs_base64,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateDiffClassV1 {
    SourceOverlay,
    BuildOutput,
    SeedState,
    UserState,
    Temporary,
    Sensitive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDiffEntryV1 {
    pub path: WorkspacePathV1,
    pub content_digest: Option<String>,
    pub observer_hint: Option<StateDiffClassV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifiedStateDiffEntryV1 {
    pub path: WorkspacePathV1,
    pub class: StateDiffClassV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
    pub include_in_seal: bool,
    pub user_confirmation_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifiedStateDiffV1 {
    pub entries: Vec<ClassifiedStateDiffEntryV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_state_artifact_id: Option<String>,
}

impl ClassifiedStateDiffV1 {
    pub fn validate_for_seal(&self) -> Result<(), AuthoringEvidenceError> {
        if self.entries.iter().any(|entry| {
            matches!(
                entry.class,
                StateDiffClassV1::Sensitive | StateDiffClassV1::Unknown
            )
        }) {
            return Err(AuthoringEvidenceError::BlockingStateDiff);
        }
        if self.entries.iter().any(|entry| {
            entry.include_in_seal
                && matches!(
                    entry.class,
                    StateDiffClassV1::UserState
                        | StateDiffClassV1::Temporary
                        | StateDiffClassV1::Sensitive
                        | StateDiffClassV1::Unknown
                )
        }) {
            return Err(AuthoringEvidenceError::ForbiddenStateInclusion);
        }
        if self.entries.iter().any(|entry| {
            entry.class == StateDiffClassV1::SeedState && entry.user_confirmation_required
        }) {
            return Err(AuthoringEvidenceError::SeedStateNotConfirmed);
        }
        Ok(())
    }
}

pub fn confirm_seed_state(
    diff: &mut ClassifiedStateDiffV1,
    seed_state_artifact_id: String,
) -> Result<(), AuthoringEvidenceError> {
    validate_digest("seed_state_artifact_id", &seed_state_artifact_id)?;
    if !diff
        .entries
        .iter()
        .any(|entry| entry.class == StateDiffClassV1::SeedState)
    {
        return Err(AuthoringEvidenceError::NoSeedState);
    }
    for entry in &mut diff.entries {
        if entry.class == StateDiffClassV1::SeedState {
            entry.user_confirmation_required = false;
            entry.include_in_seal = true;
        }
    }
    diff.seed_state_artifact_id = Some(seed_state_artifact_id);
    Ok(())
}

pub fn classify_state_diff(
    entries: &[StateDiffEntryV1],
    build_output_roots: &[WorkspacePathV1],
) -> Result<ClassifiedStateDiffV1, AuthoringEvidenceError> {
    let mut classified = entries
        .iter()
        .map(|entry| {
            let class = classify_path(entry, build_output_roots);
            if let Some(digest) = &entry.content_digest {
                validate_digest("state_diff.content_digest", digest)?;
            }
            Ok(ClassifiedStateDiffEntryV1 {
                path: entry.path.clone(),
                class,
                content_digest: entry.content_digest.clone(),
                include_in_seal: matches!(
                    class,
                    StateDiffClassV1::SourceOverlay | StateDiffClassV1::BuildOutput
                ),
                user_confirmation_required: class == StateDiffClassV1::SeedState,
            })
        })
        .collect::<Result<Vec<_>, AuthoringEvidenceError>>()?;
    classified.sort_by(|a, b| a.path.cmp(&b.path));
    if classified
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path)
    {
        return Err(AuthoringEvidenceError::DuplicateStatePath);
    }
    Ok(ClassifiedStateDiffV1 {
        entries: classified,
        seed_state_artifact_id: None,
    })
}

fn classify_path(entry: &StateDiffEntryV1, outputs: &[WorkspacePathV1]) -> StateDiffClassV1 {
    let path = entry.path.as_str();
    if looks_sensitive(path) {
        return StateDiffClassV1::Sensitive;
    }
    if is_under_any(path, outputs) {
        return StateDiffClassV1::BuildOutput;
    }
    if is_temporary(path) {
        return StateDiffClassV1::Temporary;
    }
    match entry.observer_hint {
        Some(StateDiffClassV1::SourceOverlay) => StateDiffClassV1::SourceOverlay,
        Some(StateDiffClassV1::SeedState) => StateDiffClassV1::SeedState,
        Some(StateDiffClassV1::UserState) => StateDiffClassV1::UserState,
        // An observer may make a classification stricter, never claim an
        // undeclared path is a build output.
        Some(StateDiffClassV1::Sensitive) => StateDiffClassV1::Sensitive,
        Some(StateDiffClassV1::Temporary) => StateDiffClassV1::Temporary,
        _ => StateDiffClassV1::Unknown,
    }
}

fn is_under_any(path: &str, roots: &[WorkspacePathV1]) -> bool {
    roots.iter().any(|root| {
        path == root.as_str()
            || path
                .strip_prefix(root.as_str())
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn looks_sensitive(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower == ".env"
        || lower.ends_with("/.env")
        || lower.contains("/.ssh/")
        || lower.contains(".aws/")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with("credentials")
}

fn is_temporary(path: &str) -> bool {
    path == ".tmp"
        || path.starts_with(".tmp/")
        || path == "tmp"
        || path.starts_with("tmp/")
        || path.ends_with(".tmp")
        || path.contains("/.cache/")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyStateSealRequestV1 {
    pub capsule_revision_id: String,
    pub materialization_plan_id: String,
    pub clean_replay_receipt: CleanReplayReceiptV1,
    pub classified_state_diff: ClassifiedStateDiffV1,
    pub selected_screenshot_candidate_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealCaptureObservationV1 {
    pub receipt_id: String,
    pub ready_before_capture: bool,
    pub quiesced: bool,
    pub rootfs_artifact_ref: String,
    pub memory_artifact_ref: String,
    pub runner_hardware_compatibility_class: String,
    pub guest_kernel: String,
    pub vmm: String,
    pub snapshot_format: String,
    pub restore_verification: RestoreVerificationObservationV1,
    pub post_restore_screenshot: ScreenshotCandidateV1,
    pub issued_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreVerificationReceiptV1 {
    pub payload_jcs_base64: String,
    pub authentication: BuilderAuthenticationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreVerificationReceiptPayloadV1 {
    pub schema: String,
    pub receipt_id: String,
    pub authoring_session_id: String,
    pub capsule_revision_id: String,
    pub source_closure_id: String,
    pub program_intent_digest: String,
    pub previous_receipt_digest: String,
    pub restored: bool,
    pub readiness_succeeded: bool,
    pub verified_at: String,
    pub issued_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreVerificationObservationV1 {
    pub receipt_id: String,
    pub restored: bool,
    pub readiness_succeeded: bool,
    pub verified_at: String,
    pub issued_at: String,
    pub expires_at: String,
}

pub trait ReadyStateSealAdapter {
    fn capture_and_verify(
        &mut self,
        request: &ReadyStateSealRequestV1,
    ) -> Result<SealCaptureObservationV1, String>;
    fn authenticate(&mut self, payload: &[u8]) -> Result<BuilderAuthenticationV1, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyStateSealReceiptV1 {
    pub payload_jcs_base64: String,
    pub restore_verification_receipt: RestoreVerificationReceiptV1,
    pub authentication: BuilderAuthenticationV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyStateSealReceiptPayloadV1 {
    pub schema: String,
    pub receipt_id: String,
    pub seal_id: String,
    pub authoring_session_id: String,
    pub capsule_revision_id: String,
    pub source_closure_id: String,
    pub program_intent_digest: String,
    pub previous_receipt_digest: String,
    pub materialization_plan_id: String,
    pub builder_identity: String,
    pub runner_hardware_compatibility_class: String,
    pub guest_kernel: String,
    pub vmm: String,
    pub snapshot_format: String,
    pub rootfs_artifact_ref: String,
    pub memory_artifact_ref: String,
    pub clean_replay_receipt_digest: String,
    pub restore_verification_receipt_digest: String,
    pub post_restore_screenshot: ScreenshotCandidateV1,
    pub issued_at: String,
    pub expires_at: String,
}

pub fn generate_ready_state_seal(
    adapter: &mut impl ReadyStateSealAdapter,
    request: &ReadyStateSealRequestV1,
) -> Result<ReadyStateSealReceiptV1, AuthoringEvidenceError> {
    verify_replay_binding(request)?;
    request.classified_state_diff.validate_for_seal()?;
    if request.selected_screenshot_candidate_id.trim().is_empty() {
        return Err(AuthoringEvidenceError::ScreenshotNotSelected);
    }
    let observation = adapter
        .capture_and_verify(request)
        .map_err(AuthoringEvidenceError::Adapter)?;
    if !observation.ready_before_capture {
        return Err(AuthoringEvidenceError::SealBeforeReadiness);
    }
    if !observation.quiesced {
        return Err(AuthoringEvidenceError::NotQuiesced);
    }
    if !observation.restore_verification.restored
        || !observation.restore_verification.readiness_succeeded
    {
        return Err(AuthoringEvidenceError::RestoreVerificationFailed);
    }
    if observation.post_restore_screenshot.capture_point
        != ScreenshotCapturePointV1::RestoreVerification
    {
        return Err(AuthoringEvidenceError::MissingPostRestoreScreenshot);
    }
    let replay_payload = request.clean_replay_receipt.payload()?;
    let replay_digest = request.clean_replay_receipt.payload_digest()?;
    let restore_payload = RestoreVerificationReceiptPayloadV1 {
        schema: RESTORE_VERIFICATION_RECEIPT_V1_SCHEMA.to_string(),
        receipt_id: observation.restore_verification.receipt_id,
        authoring_session_id: replay_payload.authoring_session_id.clone(),
        capsule_revision_id: replay_payload.capsule_revision_id.clone(),
        source_closure_id: replay_payload.source_closure_id.clone(),
        program_intent_digest: replay_payload.program_intent_digest.clone(),
        previous_receipt_digest: replay_digest.clone(),
        restored: observation.restore_verification.restored,
        readiness_succeeded: observation.restore_verification.readiness_succeeded,
        verified_at: observation.restore_verification.verified_at,
        issued_at: observation.restore_verification.issued_at,
        expires_at: observation.restore_verification.expires_at,
    };
    validate_receipt_chain(
        &restore_payload.receipt_id,
        &restore_payload.authoring_session_id,
        &restore_payload.capsule_revision_id,
        &restore_payload.source_closure_id,
        &restore_payload.previous_receipt_digest,
        &restore_payload.issued_at,
        &restore_payload.expires_at,
    )?;
    let restore_canonical = canonical_payload(&restore_payload)?;
    let restore_authentication = adapter
        .authenticate(&restore_canonical)
        .map_err(AuthoringEvidenceError::Adapter)?;
    let restore_receipt = RestoreVerificationReceiptV1 {
        payload_jcs_base64: BASE64.encode(&restore_canonical),
        authentication: restore_authentication,
    };
    let restore_digest = restore_receipt.payload_digest()?;
    let seal_id = canonical_digest(
        b"ato.ready-state-seal/v1",
        &SealIdentityMaterialV1 {
            capsule_revision_id: &request.capsule_revision_id,
            rootfs_artifact_ref: &observation.rootfs_artifact_ref,
            memory_artifact_ref: &observation.memory_artifact_ref,
            clean_replay_receipt_digest: &replay_digest,
            restore_verification_receipt_digest: &restore_digest,
        },
    )?;
    let payload = ReadyStateSealReceiptPayloadV1 {
        schema: READY_STATE_SEAL_RECEIPT_V1_SCHEMA.to_string(),
        receipt_id: observation.receipt_id,
        seal_id,
        authoring_session_id: replay_payload.authoring_session_id,
        capsule_revision_id: request.capsule_revision_id.clone(),
        source_closure_id: replay_payload.source_closure_id,
        program_intent_digest: replay_payload.program_intent_digest,
        previous_receipt_digest: restore_digest.clone(),
        materialization_plan_id: request.materialization_plan_id.clone(),
        builder_identity: replay_payload.builder_identity,
        runner_hardware_compatibility_class: observation.runner_hardware_compatibility_class,
        guest_kernel: observation.guest_kernel,
        vmm: observation.vmm,
        snapshot_format: observation.snapshot_format,
        rootfs_artifact_ref: observation.rootfs_artifact_ref,
        memory_artifact_ref: observation.memory_artifact_ref,
        clean_replay_receipt_digest: replay_digest,
        restore_verification_receipt_digest: restore_digest,
        post_restore_screenshot: observation.post_restore_screenshot,
        issued_at: observation.issued_at,
        expires_at: observation.expires_at,
    };
    validate_receipt_chain(
        &payload.receipt_id,
        &payload.authoring_session_id,
        &payload.capsule_revision_id,
        &payload.source_closure_id,
        &payload.previous_receipt_digest,
        &payload.issued_at,
        &payload.expires_at,
    )?;
    let canonical = canonical_payload(&payload)?;
    let authentication = adapter
        .authenticate(&canonical)
        .map_err(AuthoringEvidenceError::Adapter)?;
    Ok(ReadyStateSealReceiptV1 {
        payload_jcs_base64: BASE64.encode(canonical),
        restore_verification_receipt: restore_receipt,
        authentication,
    })
}

#[derive(Debug, Serialize)]
struct SealIdentityMaterialV1<'a> {
    capsule_revision_id: &'a str,
    rootfs_artifact_ref: &'a str,
    memory_artifact_ref: &'a str,
    clean_replay_receipt_digest: &'a str,
    restore_verification_receipt_digest: &'a str,
}

impl RestoreVerificationReceiptV1 {
    pub fn payload(&self) -> Result<RestoreVerificationReceiptPayloadV1, AuthoringEvidenceError> {
        decode_payload(&self.payload_jcs_base64)
    }

    pub fn payload_digest(&self) -> Result<String, AuthoringEvidenceError> {
        signed_payload_digest(
            b"ato.restore-verification-receipt-reference/v1",
            &self.payload_jcs_base64,
        )
    }
}

impl ReadyStateSealReceiptV1 {
    pub fn payload(&self) -> Result<ReadyStateSealReceiptPayloadV1, AuthoringEvidenceError> {
        decode_payload(&self.payload_jcs_base64)
    }

    pub fn payload_digest(&self) -> Result<String, AuthoringEvidenceError> {
        signed_payload_digest(
            b"ato.ready-state-seal-receipt-reference/v1",
            &self.payload_jcs_base64,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotCapturePointV1 {
    Readiness,
    FirstMeaningfulFrame,
    PrimaryNavigation,
    InteractionIdle,
    BeforeSaveReadyState,
    RestoreVerification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotCandidateV1 {
    pub candidate_id: String,
    pub artifact_ref: String,
    pub perceptual_hash: String,
    pub capture_point: ScreenshotCapturePointV1,
    pub quality_score: u16,
    pub possible_personal_data: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenshotFrameSignalsV1 {
    pub blank: bool,
    pub loading: bool,
    pub error_surface: bool,
    pub meaningful_pixel_ratio_per_mille: u16,
    pub possible_personal_data: bool,
}

pub fn score_screenshot_frame(signals: ScreenshotFrameSignalsV1) -> u16 {
    let mut score = signals.meaningful_pixel_ratio_per_mille.min(1000);
    if signals.blank {
        score = score.min(10);
    }
    if signals.loading {
        score = score.saturating_sub(400);
    }
    if signals.error_surface {
        score = score.saturating_sub(700);
    }
    if signals.possible_personal_data {
        score = score.saturating_sub(200);
    }
    score
}

pub fn deduplicate_screenshot_candidates(
    candidates: Vec<ScreenshotCandidateV1>,
) -> Vec<ScreenshotCandidateV1> {
    let mut seen = BTreeSet::new();
    let mut deduplicated = Vec::new();
    for candidate in candidates {
        if seen.insert(candidate.perceptual_hash.clone()) {
            deduplicated.push(candidate);
        }
    }
    deduplicated
}

/// Compute the SSOT screenshot duplicate key from compositor PNG bytes.
///
/// dHash intentionally ignores small encoding and color differences while
/// retaining the coarse visual structure of the frame.
pub fn screenshot_perceptual_hash_png(png: &[u8]) -> Result<String, AuthoringEvidenceError> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|_| AuthoringEvidenceError::InvalidScreenshot)?
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = 0_u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if image.get_pixel(x, y)[0] > image.get_pixel(x + 1, y)[0] {
                hash |= 1;
            }
        }
    }
    Ok(format!("dhash64:{hash:016x}"))
}

fn validate_replay_request(request: &CleanReplayRequestV1) -> Result<(), AuthoringEvidenceError> {
    for (field, value) in [
        (
            "authoring_session_id",
            request.authoring_session_id.as_str(),
        ),
        ("capsule_revision_id", request.capsule_revision_id.as_str()),
        ("source_closure_id", request.source_closure_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(AuthoringEvidenceError::Missing(field));
        }
    }
    validate_digest(
        "normalized_program_intent.digest",
        &request.normalized_program_intent.digest,
    )?;
    validate_digest("resolution_lock_digest", &request.resolution_lock_digest)?;
    validate_digest("previous_receipt_digest", &request.previous_receipt_digest)?;
    for overlay in &request.source_overlays {
        validate_digest("source_overlay.content_digest", &overlay.content_digest)?;
    }
    for digest in &request.allowed_cache_digests {
        validate_digest("allowed_cache_digests[]", digest)?;
    }
    Ok(())
}

fn verify_replay_binding(request: &ReadyStateSealRequestV1) -> Result<(), AuthoringEvidenceError> {
    let receipt = &request.clean_replay_receipt;
    let payload = receipt.payload()?;
    if payload.schema != CLEAN_REPLAY_RECEIPT_V1_SCHEMA
        || !payload.readiness.ready
        || receipt.authentication.signature.trim().is_empty()
        || payload.capsule_revision_id != request.capsule_revision_id
    {
        return Err(AuthoringEvidenceError::InvalidCleanReplayReceipt);
    }
    payload.effective_isolation_posture.validate()
}

fn validate_receipt_chain(
    receipt_id: &str,
    authoring_session_id: &str,
    capsule_revision_id: &str,
    source_closure_id: &str,
    previous_receipt_digest: &str,
    issued_at: &str,
    expires_at: &str,
) -> Result<(), AuthoringEvidenceError> {
    for (field, value) in [
        ("receipt_id", receipt_id),
        ("authoring_session_id", authoring_session_id),
        ("capsule_revision_id", capsule_revision_id),
        ("source_closure_id", source_closure_id),
        ("issued_at", issued_at),
        ("expires_at", expires_at),
    ] {
        if value.trim().is_empty() {
            return Err(AuthoringEvidenceError::Missing(field));
        }
    }
    validate_digest("previous_receipt_digest", previous_receipt_digest)
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), AuthoringEvidenceError> {
    let Some(hex) = value.strip_prefix("blake3:") else {
        return Err(AuthoringEvidenceError::InvalidDigest(field));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AuthoringEvidenceError::InvalidDigest(field));
    }
    Ok(())
}

fn canonical_digest(
    domain: &[u8],
    value: &impl Serialize,
) -> Result<String, AuthoringEvidenceError> {
    let canonical = serde_jcs::to_vec(value)
        .map_err(|error| AuthoringEvidenceError::Canonicalization(error.to_string()))?;
    Ok(digest_bytes(domain, &canonical))
}

fn canonical_payload(value: &impl Serialize) -> Result<Vec<u8>, AuthoringEvidenceError> {
    serde_jcs::to_vec(value)
        .map_err(|error| AuthoringEvidenceError::Canonicalization(error.to_string()))
}

fn decode_payload<T: DeserializeOwned>(
    payload_jcs_base64: &str,
) -> Result<T, AuthoringEvidenceError> {
    let bytes = BASE64
        .decode(payload_jcs_base64)
        .map_err(|_| AuthoringEvidenceError::InvalidSignedPayload)?;
    serde_json::from_slice(&bytes).map_err(|_| AuthoringEvidenceError::InvalidSignedPayload)
}

fn signed_payload_digest(
    domain: &[u8],
    payload_jcs_base64: &str,
) -> Result<String, AuthoringEvidenceError> {
    let bytes = BASE64
        .decode(payload_jcs_base64)
        .map_err(|_| AuthoringEvidenceError::InvalidSignedPayload)?;
    Ok(digest_bytes(domain, &bytes))
}

fn digest_bytes(domain: &[u8], payload: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&[0]);
    hasher.update(payload);
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthoringEvidenceError {
    #[error("missing required field {0}")]
    Missing(&'static str),
    #[error("invalid BLAKE3 digest in {0}")]
    InvalidDigest(&'static str),
    #[error("builder adapter failed: {0}")]
    Adapter(String),
    #[error("clean replay isolation posture is insufficient")]
    InsufficientIsolation,
    #[error("clean replay readiness failed")]
    ReadinessFailed,
    #[error("duplicate state diff path")]
    DuplicateStatePath,
    #[error("state diff contains sensitive or unknown paths")]
    BlockingStateDiff,
    #[error("state class may not be included in a public Seal")]
    ForbiddenStateInclusion,
    #[error("seed state requires explicit confirmation and artifact identity")]
    SeedStateNotConfirmed,
    #[error("there is no seed state to confirm")]
    NoSeedState,
    #[error("clean replay receipt is absent or invalid")]
    InvalidCleanReplayReceipt,
    #[error("cannot seal before readiness")]
    SealBeforeReadiness,
    #[error("workload was not quiesced before capture")]
    NotQuiesced,
    #[error("restore verification failed")]
    RestoreVerificationFailed,
    #[error("post-restore screenshot is required")]
    MissingPostRestoreScreenshot,
    #[error("a screenshot candidate must be selected")]
    ScreenshotNotSelected,
    #[error("screenshot is not a decodable PNG")]
    InvalidScreenshot,
    #[error("canonicalization failed: {0}")]
    Canonicalization(String),
    #[error("signed builder receipt payload is not valid base64-encoded JSON")]
    InvalidSignedPayload,
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::authoring_intent::{
        NORMALIZED_PROGRAM_INTENT_V1_SCHEMA, NormalizedProgramCommandV1, NormalizedProgramIntentV1,
        ReadinessIntentV1,
    };

    fn digest(ch: char) -> String {
        format!("blake3:{}", ch.to_string().repeat(64))
    }

    fn request() -> CleanReplayRequestV1 {
        CleanReplayRequestV1 {
            authoring_session_id: "as_1".to_string(),
            capsule_revision_id: "revision_1".to_string(),
            source_closure_id: "sc_1".to_string(),
            previous_receipt_digest: digest('9'),
            source_overlays: Vec::new(),
            normalized_program_intent: NormalizedProgramIntentEnvelopeV1 {
                intent: NormalizedProgramIntentV1 {
                    schema: NORMALIZED_PROGRAM_INTENT_V1_SCHEMA.to_string(),
                    toolchains: Vec::new(),
                    build_steps: Vec::new(),
                    launch: NormalizedProgramCommandV1 {
                        argv: vec!["node".to_string(), "server.js".to_string()],
                        cwd: WorkspacePathV1::root(),
                        requested_environment: Vec::new(),
                        required_tools: Vec::new(),
                        explicit_shell_escape: false,
                    },
                    readiness: ReadinessIntentV1::Tcp {
                        port: 8000,
                        timeout_seconds: 60,
                    },
                    build_output_roots: vec![WorkspacePathV1::parse("dist").expect("path")],
                    bindings: Vec::new(),
                },
                digest: digest('a'),
            },
            resolution_lock_digest: digest('b'),
            allowed_cache_digests: vec![digest('c')],
        }
    }

    struct ReplayAdapter {
        isolation: EffectiveIsolationPostureV1,
        diff: Vec<StateDiffEntryV1>,
    }

    impl CleanReplayAdapter for ReplayAdapter {
        fn replay(&mut self, _: &CleanReplayRequestV1) -> Result<CleanReplayObservationV1, String> {
            Ok(CleanReplayObservationV1 {
                receipt_id: "replay_1".to_string(),
                builder_identity: "builder:test".to_string(),
                materialization_inputs_digest: digest('d'),
                execution_contract_digest: digest('e'),
                readiness: ReadinessResultV1 {
                    ready: true,
                    probe_digest: digest('f'),
                    observed_at: "2026-07-28T00:00:01Z".to_string(),
                },
                isolation: self.isolation.clone(),
                state_diff: self.diff.clone(),
                started_at: "2026-07-28T00:00:00Z".to_string(),
                completed_at: "2026-07-28T00:00:02Z".to_string(),
                issued_at: "2026-07-28T00:00:02Z".to_string(),
                expires_at: "2026-07-28T00:15:02Z".to_string(),
            })
        }

        fn authenticate(&mut self, _: &[u8]) -> Result<BuilderAuthenticationV1, String> {
            Ok(BuilderAuthenticationV1 {
                key_id: "key:test".to_string(),
                algorithm: "ed25519".to_string(),
                signature: "signed-by-builder".to_string(),
            })
        }
    }

    fn isolated() -> EffectiveIsolationPostureV1 {
        EffectiveIsolationPostureV1 {
            ephemeral_workspace: true,
            host_filesystem_hidden: true,
            host_environment_inherited: false,
            host_credentials_inherited: false,
            privileged: false,
            network_observed: true,
            workspace_provenance: "fresh:nonce-1".to_string(),
        }
    }

    fn successful_replay_receipt() -> CleanReplayReceiptV1 {
        let mut adapter = ReplayAdapter {
            isolation: isolated(),
            diff: Vec::new(),
        };
        execute_clean_replay(&mut adapter, &request())
            .expect("receipt")
            .0
    }

    struct SealAdapter {
        ready: bool,
        restored: bool,
    }

    impl ReadyStateSealAdapter for SealAdapter {
        fn capture_and_verify(
            &mut self,
            _: &ReadyStateSealRequestV1,
        ) -> Result<SealCaptureObservationV1, String> {
            Ok(SealCaptureObservationV1 {
                receipt_id: "seal_receipt_1".to_string(),
                ready_before_capture: self.ready,
                quiesced: true,
                rootfs_artifact_ref: "cas:rootfs".to_string(),
                memory_artifact_ref: "cas:memory".to_string(),
                runner_hardware_compatibility_class: "fc-arm64-v1".to_string(),
                guest_kernel: "linux-6.12".to_string(),
                vmm: "firecracker-1.12".to_string(),
                snapshot_format: "fc-v1".to_string(),
                restore_verification: RestoreVerificationObservationV1 {
                    receipt_id: "restore_1".to_string(),
                    restored: self.restored,
                    readiness_succeeded: self.restored,
                    verified_at: "2026-07-28T00:01:00Z".to_string(),
                    issued_at: "2026-07-28T00:01:00Z".to_string(),
                    expires_at: "2026-07-28T00:16:00Z".to_string(),
                },
                post_restore_screenshot: ScreenshotCandidateV1 {
                    candidate_id: "shot_post_restore".to_string(),
                    artifact_ref: "cas:screenshot".to_string(),
                    perceptual_hash: "phash:1".to_string(),
                    capture_point: ScreenshotCapturePointV1::RestoreVerification,
                    quality_score: 800,
                    possible_personal_data: false,
                },
                issued_at: "2026-07-28T00:01:01Z".to_string(),
                expires_at: "2026-07-28T00:16:01Z".to_string(),
            })
        }

        fn authenticate(&mut self, _: &[u8]) -> Result<BuilderAuthenticationV1, String> {
            Ok(BuilderAuthenticationV1 {
                key_id: "key:test".to_string(),
                algorithm: "ed25519".to_string(),
                signature: "seal-signature".to_string(),
            })
        }
    }

    #[test]
    fn clean_replay_receipt_is_builder_authenticated() {
        let mut adapter = ReplayAdapter {
            isolation: isolated(),
            diff: vec![StateDiffEntryV1 {
                path: WorkspacePathV1::parse("dist/app.js").expect("path"),
                content_digest: Some(digest('1')),
                observer_hint: None,
            }],
        };
        let (receipt, classified) =
            execute_clean_replay(&mut adapter, &request()).expect("receipt");
        assert_eq!(receipt.authentication.signature, "signed-by-builder");
        let payload = receipt.payload().expect("payload");
        assert_eq!(payload.receipt_id, "replay_1");
        assert_eq!(payload.capsule_revision_id, "revision_1");
        assert_eq!(payload.previous_receipt_digest, digest('9'));
        assert_eq!(classified.entries[0].class, StateDiffClassV1::BuildOutput);
        assert!(classified.entries[0].include_in_seal);
    }

    #[test]
    fn authoring_workspace_disguised_as_replay_is_rejected() {
        let mut posture = isolated();
        posture.ephemeral_workspace = false;
        posture.workspace_provenance = "authoring:as_1".to_string();
        let mut adapter = ReplayAdapter {
            isolation: posture,
            diff: Vec::new(),
        };
        assert_eq!(
            execute_clean_replay(&mut adapter, &request()),
            Err(AuthoringEvidenceError::InsufficientIsolation)
        );
    }

    #[test]
    fn unknown_and_sensitive_diff_fail_closed() {
        let diff = classify_state_diff(
            &[
                StateDiffEntryV1 {
                    path: WorkspacePathV1::parse("mystery/file").expect("path"),
                    content_digest: Some(digest('1')),
                    observer_hint: None,
                },
                StateDiffEntryV1 {
                    path: WorkspacePathV1::parse(".env").expect("path"),
                    content_digest: Some(digest('2')),
                    observer_hint: None,
                },
            ],
            &[],
        )
        .expect("classification");
        assert_eq!(diff.entries[0].class, StateDiffClassV1::Sensitive);
        assert_eq!(diff.entries[1].class, StateDiffClassV1::Unknown);
        assert_eq!(
            diff.validate_for_seal(),
            Err(AuthoringEvidenceError::BlockingStateDiff)
        );
    }

    #[test]
    fn screenshot_candidates_are_deduplicated_by_perceptual_hash() {
        let candidate = |id: &str, hash: &str| ScreenshotCandidateV1 {
            candidate_id: id.to_string(),
            artifact_ref: format!("cas:{id}"),
            perceptual_hash: hash.to_string(),
            capture_point: ScreenshotCapturePointV1::Readiness,
            quality_score: 50,
            possible_personal_data: false,
        };
        let result = deduplicate_screenshot_candidates(vec![
            candidate("a", "same"),
            candidate("b", "same"),
            candidate("c", "other"),
        ]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].candidate_id, "a");
    }

    #[test]
    fn screenshot_perceptual_hash_is_stable_for_the_same_frame() {
        let mut frame = image::GrayImage::new(9, 8);
        for (x, _, pixel) in frame.enumerate_pixels_mut() {
            pixel.0[0] = (x * 20) as u8;
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(frame)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("png");
        assert_eq!(
            screenshot_perceptual_hash_png(png.get_ref()).expect("hash"),
            screenshot_perceptual_hash_png(png.get_ref()).expect("hash"),
        );
    }

    #[test]
    fn seed_state_requires_confirmation_and_separate_identity() {
        let mut diff = classify_state_diff(
            &[StateDiffEntryV1 {
                path: WorkspacePathV1::parse("data/seed.db").expect("path"),
                content_digest: Some(digest('3')),
                observer_hint: Some(StateDiffClassV1::SeedState),
            }],
            &[],
        )
        .expect("classification");
        assert_eq!(
            diff.validate_for_seal(),
            Err(AuthoringEvidenceError::SeedStateNotConfirmed)
        );
        confirm_seed_state(&mut diff, digest('4')).expect("confirmation");
        assert!(diff.validate_for_seal().is_ok());
        assert_eq!(diff.seed_state_artifact_id, Some(digest('4')));
    }

    #[test]
    fn blank_loading_and_error_frames_score_low() {
        let score = score_screenshot_frame(ScreenshotFrameSignalsV1 {
            blank: true,
            loading: true,
            error_surface: true,
            meaningful_pixel_ratio_per_mille: 900,
            possible_personal_data: false,
        });
        assert_eq!(score, 0);
    }

    #[test]
    fn seal_requires_clean_replay_readiness_and_restore_verification() {
        let request = ReadyStateSealRequestV1 {
            capsule_revision_id: "revision_1".to_string(),
            materialization_plan_id: "plan_1".to_string(),
            clean_replay_receipt: successful_replay_receipt(),
            classified_state_diff: ClassifiedStateDiffV1 {
                entries: Vec::new(),
                seed_state_artifact_id: None,
            },
            selected_screenshot_candidate_id: "shot_before_save".to_string(),
        };
        let mut adapter = SealAdapter {
            ready: true,
            restored: true,
        };
        let receipt = generate_ready_state_seal(&mut adapter, &request).expect("seal");
        let payload = receipt.payload().expect("seal payload");
        assert!(payload.seal_id.starts_with("blake3:"));
        let restore_payload = receipt
            .restore_verification_receipt
            .payload()
            .expect("restore payload");
        assert_eq!(
            payload.previous_receipt_digest,
            receipt
                .restore_verification_receipt
                .payload_digest()
                .expect("restore digest")
        );
        assert_eq!(
            restore_payload.previous_receipt_digest,
            payload.clean_replay_receipt_digest
        );
        assert_eq!(
            payload.post_restore_screenshot.capture_point,
            ScreenshotCapturePointV1::RestoreVerification
        );

        let mut failed_restore = SealAdapter {
            ready: true,
            restored: false,
        };
        assert_eq!(
            generate_ready_state_seal(&mut failed_restore, &request),
            Err(AuthoringEvidenceError::RestoreVerificationFailed)
        );
    }
}
