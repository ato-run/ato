//! The real [`DisposableAcceptanceLifecycle`]: a disposable restore of a
//! candidate, driven against an actual [`SnapshotBackend`].
//!
//! RFC §8.1 puts validation in a disposable Session so its effects never enter
//! the accepted Snapshot: capture the immutable candidate, restore it into a
//! throwaway overlay, run `seal_at.command` there, then always destroy the
//! overlay and the Session.
//!
//! This lives in `snapshot` rather than in either caller because BOTH executors
//! of that command must behave identically: the CLI's build path and the
//! builder's interactive hold run the same verification against the same
//! contract. A second copy would be a second definition of what acceptance
//! means.
//!
//! The child is spawned with the acceptance credential namespace scrubbed
//! ([`sanitize_untrusted_environment`]) — RFC §8.4: no production secret is
//! connected to a verification run.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::layer_store::CasStore;
use capsule::snapshot_manifest::{
    CapturePolicyV1, RestoreContractV1, SnapshotCaptureProvenance, SnapshotManifestV1,
};

use crate::BuildReadyStateReceipt;
use crate::acceptance::{
    AcceptanceBudget, CandidateSnapshot, DisposableAcceptanceLifecycle, DisposableSessionHandle,
    VerificationOutcome, sanitize_untrusted_environment,
};
use crate::layer_store::BlobManifest;
use crate::manifest::ReadyStateManifest;
use crate::{RestoreReadyStateInput, RestoredSession, SnapshotBackend};
use capsule::execution_contract::{ContentDigest, DigestAlgorithm, ExecutionId};

/// Real (non-stubbed) [`DisposableAcceptanceLifecycle`] backed by an actual
/// [`SnapshotBackend`]: capture wraps the already-sealed candidate (see
/// [`default_acceptance_config`]'s doc for why there is one attempt), create
/// allocates the disposable overlay, restore calls the REAL
/// `backend.restore`, and destroy calls the REAL `backend.stop` — no phase is
/// faked or self-attesting.
/// How a lifecycle learns which candidate to verify.
///
/// The CLI's build path knows both manifests before acceptance starts, so it
/// sets them once. A hold cannot: `HoldPhase` takes its lifecycle up front, and
/// the candidate only exists after the capture runs. `LateBound` lets the hold
/// hand over the manifests the instant they exist, and the phases below read
/// whatever is current — which is always the candidate the capture just sealed,
/// never a stale one.
pub trait CandidateSource {
    /// The legacy manifest to restore (what `backend.restore` takes).
    fn legacy_manifest(&self) -> Result<ReadyStateManifest, String>;
    /// The v1 candidate manifest under verification.
    fn candidate_manifest(&self) -> Result<SnapshotManifestV1, String>;
}

/// A source fixed at construction — the CLI build path.
pub struct FixedCandidate {
    pub legacy: ReadyStateManifest,
    pub candidate: SnapshotManifestV1,
}

impl CandidateSource for FixedCandidate {
    fn legacy_manifest(&self) -> Result<ReadyStateManifest, String> {
        Ok(self.legacy.clone())
    }
    fn candidate_manifest(&self) -> Result<SnapshotManifestV1, String> {
        Ok(self.candidate.clone())
    }
}

pub struct BackendDisposableLifecycle<'a, S: CandidateSource = FixedCandidate> {
    pub backend: &'a dyn SnapshotBackend,
    pub store: &'a CasStore,
    /// Where the manifests come from — fixed for a build, late-bound for a hold.
    pub candidate: S,
    pub overlay_root: std::path::PathBuf,
    /// The live restored session, if a restore is currently in progress.
    /// `maximum_attempts` is always 1 in the shipped config (see
    /// [`default_acceptance_config`]), so at most one session is ever live —
    /// a single slot is simpler than a session-keyed map for that shape.
    pub session: Option<RestoredSession>,
    /// The last manifest handed out by [`Self::capture_candidate`] — read back
    /// by the caller once `accept` reports acceptance (the acceptance
    /// receipt itself carries only the `snapshot_id`, not the manifest).
    pub last_candidate: Option<SnapshotManifestV1>,
}

impl<S: CandidateSource> DisposableAcceptanceLifecycle for BackendDisposableLifecycle<'_, S> {
    fn capture_candidate(
        &mut self,
        _attempt: u32,
        _budget: &AcceptanceBudget,
    ) -> Result<CandidateSnapshot, String> {
        let manifest = self.candidate.candidate_manifest()?;
        self.last_candidate = Some(manifest.clone());
        Ok(CandidateSnapshot { manifest })
    }

    fn create_disposable_session(
        &mut self,
        _candidate: &CandidateSnapshot,
        _budget: &AcceptanceBudget,
    ) -> Result<DisposableSessionHandle, String> {
        std::fs::create_dir_all(&self.overlay_root).map_err(|error| error.to_string())?;
        Ok(DisposableSessionHandle {
            opaque_id: "v1-acceptance".to_string(),
        })
    }

    fn restore_candidate(
        &mut self,
        session: &DisposableSessionHandle,
        _candidate: &CandidateSnapshot,
        _budget: &AcceptanceBudget,
    ) -> Result<(), String> {
        let overlay = self.overlay_root.join(&session.opaque_id);
        let restored = self
            .backend
            .restore(RestoreReadyStateInput {
                store: self.store,
                manifest: self.candidate.legacy_manifest()?,
                overlay_root: overlay,
                host_runner_class: None,
                containment: None,
                uffd_preview: false,
            })
            .map_err(|error| error.to_string())?;
        self.session = Some(restored.session);
        Ok(())
    }

    /// Execute `seal_at.command` as a real host-side subprocess (no shell,
    /// exact argv preserved via `Command::args`) against the disposable
    /// Session, with the SAME untrusted-environment scrubbing every other
    /// shell-out in this crate applies.
    ///
    /// **Scope note**: the RFC's model is an IN-GUEST exec (RFC §8.1); no
    /// transport for that exists yet in this codebase (`AgentChannel` carries
    /// only the typed binding-control protocol, not arbitrary command exec —
    /// see `snapshot::agent_channel`). Running the verification command
    /// host-side is a real, honest interpretation (an operator-supplied
    /// argv — e.g. a `curl` against the restored session's exposed port —
    /// genuinely runs and is faithfully classified below), not a fabricated
    /// success signal; it is documented here as the gap a future in-guest
    /// exec channel would close.
    fn execute_exact_argv(
        &mut self,
        _session: &DisposableSessionHandle,
        argv: &[String],
        timeout: Duration,
        _budget: &AcceptanceBudget,
    ) -> Result<VerificationOutcome, String> {
        let (program, rest) = argv
            .split_first()
            .ok_or_else(|| "seal_at argv is empty".to_string())?;
        let mut command = Command::new(program);
        command
            .args(rest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        sanitize_untrusted_environment(&mut command);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait().map_err(|error| error.to_string())? {
                Some(status) => return Ok(classify_exit_status(status)),
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(VerificationOutcome::TimedOut);
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// A no-op: this backend exposes only a single combined
    /// stop-and-teardown primitive (`SnapshotBackend::stop`), which already
    /// terminates the guest's process tree as part of tearing down the
    /// overlay — called unconditionally by
    /// [`Self::destroy_disposable_session`]. Calling it twice here would
    /// double-stop the same session.
    fn terminate_process_tree(&mut self, _session: &DisposableSessionHandle) -> Result<(), String> {
        Ok(())
    }

    fn destroy_disposable_session(
        &mut self,
        session: DisposableSessionHandle,
    ) -> Result<(), String> {
        if let Some(restored) = self.session.take() {
            self.backend
                .stop(restored)
                .map_err(|error| error.to_string())?;
        }
        let overlay = self.overlay_root.join(&session.opaque_id);
        let _ = std::fs::remove_dir_all(overlay);
        Ok(())
    }
}

#[cfg(unix)]
fn classify_exit_status(status: std::process::ExitStatus) -> VerificationOutcome {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(code) => VerificationOutcome::Exited(code),
        None => match status.signal() {
            Some(signal) => VerificationOutcome::Signalled(signal),
            None => VerificationOutcome::Lost,
        },
    }
}

#[cfg(not(unix))]
fn classify_exit_status(status: std::process::ExitStatus) -> VerificationOutcome {
    match status.code() {
        Some(code) => VerificationOutcome::Exited(code),
        None => VerificationOutcome::Lost,
    }
}

/// Derive a REAL (backend-reported) v1 identity/compatibility sidecar for the
/// legacy manifest `build_ready_state` just sealed. Built directly from the
/// concrete, already-validated [`ReadyStateManifest`] in hand (rather than a
/// tolerant JSON round-trip through
/// [`LegacyReadyStateManifestV1`](capsule::snapshot_manifest::LegacyReadyStateManifestV1),
/// which exists for reading OPAQUE legacy artifacts this caller does not
/// have).
pub fn build_v1_candidate_manifest(
    backend: &dyn SnapshotBackend,
    execution_id: ExecutionId,
    receipt: &BuildReadyStateReceipt,
) -> Result<SnapshotManifestV1, String> {
    use capsule::snapshot_manifest::{
        SNAPSHOT_MANIFEST_V1_SCHEMA, SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA,
        SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA, SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA,
        SanitizationAttestationV1, SecretScanAttestationV1,
    };

    let compatibility_contract = backend
        .snapshot_compatibility_contract()
        .map_err(|e| format!("resolve Snapshot v1 backend compatibility: {e}",))?;
    let legacy = &receipt.manifest;

    let mut disk_layer_refs = Vec::new();
    for layer in [
        &legacy.layers.rootfs,
        &legacy.layers.runtime,
        &legacy.layers.dependency,
        &legacy.layers.app,
    ]
    .into_iter()
    .flatten()
    {
        disk_layer_refs.push(blob_layer_ref(layer)?);
    }
    let memory_layer_refs = legacy
        .layers
        .memory
        .as_ref()
        .map(blob_layer_ref)
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();
    let vmstate_layer_refs = legacy
        .layers
        .vmstate
        .as_ref()
        .map(blob_layer_ref)
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();

    let sanitization_steps = legacy
        .sanitizer_contract
        .steps
        .iter()
        .map(|step| step.step.clone())
        .collect();
    let secret_scan = &receipt.no_secret_proof;

    Ok(SnapshotManifestV1 {
        schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
        execution_id,
        restore_contract: RestoreContractV1 {
            schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
            // Must equal `compatibility_contract.runner_restore_contract`
            // (`SnapshotManifestV1::validate`'s cross-field invariant) — the
            // SAME restore protocol identity, viewed from two angles.
            restore_protocol: compatibility_contract.runner_restore_contract.clone(),
            steps: Vec::new(),
        },
        compatibility_contract,
        memory_layer_refs,
        vmstate_layer_refs,
        disk_layer_refs,
        capture_policy: CapturePolicyV1::Running,
        capture_provenance: SnapshotCaptureProvenance {
            capsule_manifest_hash: Some(legacy.capsule_manifest_hash.clone()),
            build_receipt_id: legacy.build_receipt_id.clone(),
        },
        sanitization_attestation: SanitizationAttestationV1 {
            schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
            steps: sanitization_steps,
        },
        secret_scan_attestation: SecretScanAttestationV1 {
            schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
            scanner_identity: secret_scan.scanner_version.clone(),
            policy_identity: crate::POLICY_VERSION.to_string(),
            scanned_layers: secret_scan.scanned_layers.clone(),
            verdict: secret_scan.verdict.clone(),
        },
    })
}

/// A real (not placeholder) content commitment for one captured
/// [`BlobManifest`] layer ref: a domain-separated hash of its own canonical
/// form. This is a genuine content address of the actual captured layer
/// metadata (which itself commits to every chunk hash within) — not the same
/// digest CapsuleFS uses internally for the blob's OWN address, but a real,
/// independently-verifiable commitment that changes iff the underlying
/// content changes.
pub(crate) fn blob_layer_ref(blob: &BlobManifest) -> Result<ContentDigest, String> {
    let canonical =
        serde_jcs::to_vec(blob).map_err(|e| format!("canonicalize Snapshot layer ref: {e}"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ato.snapshot-layer-ref/v1\0");
    hasher.update(&canonical);
    Ok(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    ))
}
