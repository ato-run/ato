//! The production [`CaptureAction`]: capture a candidate from a live held guest.
//!
//! This is the Firecracker-concrete half of the interactive HOLD. [`HoldPhase`]
//! decides *when* to capture (ADR-007 causality, ADR-008 epoch monotonicity);
//! this decides *what a capture is*, and it is deliberately the same thing a
//! build produces:
//!
//! ```text
//! HeldGuest::capture_candidate()   pause → snapshot/create → RESUME → seal
//!   → persist_and_locate_artifact  manifest.json beside the CAS, pack + upload
//!   → HeldCapture                  candidate_id, execution_id, snapshot_id, location
//! ```
//!
//! Both halves are shared code, not parallel copies: the seal is
//! `snapshot`'s own, and the persist/upload step is the very function
//! `process_job` uses. A held candidate therefore cannot drift from a built one.
//!
//! **The lease.** A capture is the longest single step of a hold and no control
//! poll runs inside it, so the lease would otherwise expire under a candidate
//! that is then unreportable. This drives it between every phase — before the
//! pause, after the bytes are sealed, and after the upload — because those are
//! exactly the boundaries where minutes can pass.
//!
//! **`source_lost` (ADR-012).** Whether the hold can be retried is decided by
//! whether the GUEST survived, independent of why a capture failed. That bit is
//! preserved on both outcomes, including a failure during sealing or upload.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use capsulefs::{CasStore, ContentHash};
use snapshot::{HeldCaptureFailure, HeldGuest};

use crate::hold_phase::{CaptureAction, CaptureError, HeldCapture, LeaseKeepalive};

/// The candidate the capture just sealed, handed to the verifier.
///
/// `HoldPhase` takes its lifecycle UP FRONT but the candidate only exists after
/// `CaptureAction::capture` runs, so the two need somewhere to meet. This is
/// that place: the capture writes, the disposable-restore lifecycle reads.
///
/// A `RefCell` rather than a lock because both live on the one hold thread —
/// `HoldPhase::run` is a blocking loop and calls them in strict sequence.
///
/// The value is REPLACED on every capture, never appended to: verifying a stale
/// candidate would accept one artifact and publish another. RFC §8.2 allows
/// repeated captures, so "the candidate" always means the most recent one.
pub type CapturedCandidateCell = Rc<RefCell<Option<snapshot::BuildReadyStateReceipt>>>;

/// Everything the capture needs that is not the guest itself.
pub struct CaptureContext {
    /// The claimed job's id — one half of `cas://<job_id>/<hash>`.
    pub job_id: String,
    /// The job's working directory: `manifest.json` lands beside its `cas/`.
    pub jobdir: PathBuf,
}

/// The live-guest [`CaptureAction`].
///
pub struct GuestCaptureAction<'a> {
    guest: HeldGuest<'a>,
    ctx: CaptureContext,
    /// Where each sealed candidate is published for the verifier.
    captured: CapturedCandidateCell,
}

impl<'a> GuestCaptureAction<'a> {
    pub fn new(guest: HeldGuest<'a>, ctx: CaptureContext, captured: CapturedCandidateCell) -> Self {
        Self {
            guest,
            ctx,
            captured,
        }
    }

    /// Tear the hold down. Consumes the action, so no capture can follow.
    ///
    /// Returns the [`ReleasedHold`] token that acceptance requires: by the time
    /// this returns, the VMM is killed and reaped, `net_down()` has run, and the
    /// slot lock is unlinked.
    pub fn release(self) -> ReleasedHold {
        self.guest.release();
        ReleasedHold(())
    }
}

/// Proof that the held guest is gone and its slot is free.
///
/// Minted only by [`GuestCaptureAction::release`], which takes `self` by value
/// and calls `HeldGuest::release` — that kills and reaps the VMM, runs
/// `net_down()`, and drops the `BuildLock`, unlinking `work_root/{netns|tap}.lock`.
///
/// [`crate::hold_phase::verify_captured_candidate`] demands one because the
/// Firecracker backend admits exactly ONE VMM per network identity, and the
/// consequences of ignoring that are not confined to the lock:
///
/// * the restore takes the same lock and fails
///   `single-session backend busy` (`firecracker.rs` `acquire_lock`);
/// * `net_up_root`'s FIRST statement is `ip link del <tap>`, which would delete
///   the tap the author's guest is attached to;
/// * both guests carry the same `guest_ip`, baked into the restored memory image
///   by the kernel cmdline, so readiness cannot tell them apart — a verify could
///   be satisfied by the HELD guest and accept a candidate it never probed;
/// * the per-capsule vsock UDS path is deterministic, so the restore unlinks the
///   live hold's socket.
///
/// Only the first of those fails loudly. Requiring this token makes the ordering
/// a compile-time obligation rather than a comment somebody has to find.
pub struct ReleasedHold(());

#[cfg(test)]
impl ReleasedHold {
    /// TEST-ONLY: stand in for a release that a fake harness performed itself.
    pub(crate) fn for_test() -> Self {
        Self(())
    }
}

/// Fail a capture while preserving the ADR-012 bit.
fn capture_failed(source_lost: bool, message: String) -> CaptureError {
    CaptureError {
        source_lost,
        message,
    }
}

/// The job CAS a hold seals into: `<jobdir>/cas`, the very store
/// `process_interactive_capture_job` opens and hands to `boot_and_hold`.
///
/// Derived rather than plumbed because the cleanup below is a FAILURE path: it
/// must work even when the attempt died before anything it could have carried a
/// handle through, and the layout is already fixed by the caller.
fn job_cas_root(jobdir: &Path) -> PathBuf {
    jobdir.join("cas")
}

/// Every chunk resident in the job CAS right now — the set a failed attempt is
/// expected to leave exactly as it found.
///
/// Taken at the START of each attempt, so it names the build's own layers
/// (rootfs, runtime, dependency, app) plus anything an earlier attempt was
/// allowed to keep. Best-effort: an unreadable store yields `None`, which
/// disables the cleanup rather than risking a removal against an unknown
/// baseline.
fn resident_chunks(jobdir: &Path) -> Option<HashSet<ContentHash>> {
    let store = CasStore::open(job_cas_root(jobdir)).ok()?;
    Some(store.list_chunks().ok()?.into_iter().collect())
}

/// #1160 — drop the bytes a FAILED capture attempt wrote into the job CAS.
///
/// A capture seals a full memory + vmstate image into the job's content-addressed
/// store, and guest memory differs on every capture, so nothing dedupes: each
/// failed attempt adds another whole memory image that no manifest will ever
/// reference. Bounding the attempt count (`MAX_CAPTURE_ATTEMPTS`) bounds how many
/// can pile up; this removes them, so a hold that burns its whole budget costs
/// the disk of ZERO candidates rather than three.
///
/// Safe by construction, and the two facts that make it so are worth stating:
///
/// * an attempt only reaches this path having produced NO reportable candidate,
///   and a successful capture ENDS the hold — so at this moment there is no live
///   candidate whose chunks could be caught in the sweep; and
/// * `pinned` is the pre-attempt residency set, so the build's own layers (which
///   the NEXT attempt still needs to seal against) are retained explicitly rather
///   than by hoping they are referenced from somewhere.
///
/// Reuses `capsulefs::gc::collect_garbage` — the same reachability sweep the CAS
/// already ships — with an empty live-manifest set, so "unreachable" means
/// precisely "not pinned". Best-effort and never fatal: failing to reclaim disk
/// must not convert a retryable capture failure into a lost hold.
/// Run one capture attempt, reclaiming its scratch if it fails.
///
/// A free function taking the attempt as a closure, rather than three lines
/// inside [`CaptureAction::capture`], for one reason: `capture` cannot be
/// exercised without `/dev/kvm`, so as an inline `if outcome.is_err()` the
/// DECISION to clean up would be the only part of #1160 no test could reach —
/// and a mutation that deleted it would go green. Here the choice is testable
/// against a real CAS with a scripted attempt, and what is left untestable is
/// one call.
fn with_attempt_scratch_reclaimed<T>(
    jobdir: &Path,
    attempt: impl FnOnce() -> Result<T, CaptureError>,
) -> Result<T, CaptureError> {
    // BEFORE the attempt: what the CAS is expected to still hold afterwards if
    // this attempt fails.
    let pinned = resident_chunks(jobdir);
    let outcome = attempt();
    // A failed attempt produced no candidate anyone can reference, so whatever
    // it added to the CAS is already garbage. A SUCCESSFUL one is left alone:
    // acceptance restores the candidate from those very chunks moments later.
    if outcome.is_err()
        && let Some(pinned) = &pinned
    {
        discard_failed_attempt_scratch(jobdir, pinned);
    }
    outcome
}

fn discard_failed_attempt_scratch(jobdir: &Path, pinned: &HashSet<ContentHash>) {
    let Ok(store) = CasStore::open(job_cas_root(jobdir)) else {
        return;
    };
    match capsulefs::gc::collect_garbage(&store, &[], pinned) {
        Ok(report) if report.deleted_count() > 0 => {
            eprintln!(
                "[builder] discarded the scratch of a failed capture: {} chunk(s), {} bytes",
                report.deleted_count(),
                report.reclaimed_bytes
            );
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("[builder] could not discard failed-capture scratch: {error}");
        }
    }
}

/// Renew the lease BEFORE the guest is touched.
///
/// Everything after this point — pause, seal, upload — runs with no control poll
/// in it, so an already-dead lease has to be found here rather than after a
/// candidate exists that nobody can report.
///
/// A dead lease is `source_lost: false`: the GUEST is fine, we just may no longer
/// speak for the claim. Calling it a lost source would end an attempt whose guest
/// is still perfectly usable.
fn pre_capture_lease_gate(lease: &mut dyn LeaseKeepalive) -> Result<(), CaptureError> {
    lease
        .keepalive()
        .map_err(|f| capture_failed(false, format!("lease died before the capture: {f:?}")))
}

impl CaptureAction for GuestCaptureAction<'_> {
    /// #1160 — the outcome is produced by [`Self::capture_once`] and this decides
    /// what a FAILED one costs.
    ///
    /// The split exists so the cleanup cannot be forgotten on one error path:
    /// `capture_once` has six of them (the lease gate, the guest capture, two
    /// more lease drives, the missing execution id, and the upload), and four of
    /// those run AFTER the seal has already written a full memory image into the
    /// job CAS. Handling them one `?` at a time is how a leak gets reintroduced.
    fn capture(
        &mut self,
        capture_epoch: u64,
        candidate_id: &str,
        lease: &mut dyn LeaseKeepalive,
    ) -> Result<HeldCapture, CaptureError> {
        let jobdir = self.ctx.jobdir.clone();
        with_attempt_scratch_reclaimed(&jobdir, || {
            self.capture_once(capture_epoch, candidate_id, lease)
        })
    }
}

impl GuestCaptureAction<'_> {
    fn capture_once(
        &mut self,
        _capture_epoch: u64,
        candidate_id: &str,
        lease: &mut dyn LeaseKeepalive,
    ) -> Result<HeldCapture, CaptureError> {
        // The hold may have sat idle for a long time waiting for the author to
        // press the button.
        pre_capture_lease_gate(lease)?;

        // Pause → snapshot → resume → seal. `capture_candidate` resumes on BOTH
        // paths, so a failure here still leaves the guest running unless the
        // resume itself failed — which is what `source_lost` reports.
        let candidate = self.guest.capture_candidate().map_err(
            |HeldCaptureFailure { error, source_lost }| {
                capture_failed(source_lost, format!("capture the held guest: {error}"))
            },
        )?;
        let source_lost = candidate.source_lost;

        // Sealing is done; the guest is running again (or lost). Renew before the
        // upload, which is the other multi-minute step.
        lease.keepalive().map_err(|f| {
            capture_failed(
                source_lost,
                format!("lease died after sealing the candidate: {f:?}"),
            )
        })?;

        let manifest = &candidate.receipt.manifest;
        let artifact_manifest_hash = manifest.id();
        let execution_id = manifest
            .execution_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                // §3.6 requires the canonical Execution Identity of the captured
                // execution. A candidate that cannot name it is unreportable, so
                // this fails rather than inventing one.
                capture_failed(
                    source_lost,
                    "sealed candidate has no execution_id".to_string(),
                )
            })?
            .to_string();

        let artifact_location = crate::persist_and_locate_artifact(
            manifest,
            &self.ctx.jobdir,
            &self.ctx.job_id,
            &artifact_manifest_hash,
        )
        .map_err(|(stage, reason)| capture_failed(source_lost, format!("{stage}: {reason}")))?;

        // After the upload: the candidate report is the very next call, and it
        // carries the fencing tuple this lease belongs to.
        lease.keepalive().map_err(|f| {
            capture_failed(
                source_lost,
                format!("lease died after uploading the candidate: {f:?}"),
            )
        })?;

        // Publish BEFORE returning: `HoldPhase` runs acceptance against this
        // candidate on the very next statement, and a verifier that read a stale
        // one would accept an artifact nobody is about to publish.
        *self.captured.borrow_mut() = Some(candidate.receipt);

        Ok(HeldCapture {
            // Echoed verbatim: §3.6 has the server cross-check epoch↔candidate
            // 1:1 against the id it minted, so this must never be re-derived.
            candidate_id: candidate_id.to_string(),
            execution_id,
            // The sealed snapshot for this candidate IS the sealed artifact
            // manifest — the same id `process_job` registers.
            snapshot_id: artifact_manifest_hash,
            artifact_location,
            source_lost,
        })
    }
}

/// The hold's [`CandidateSource`]: whatever the last capture published.
///
/// `HoldPhase` takes its lifecycle before any candidate exists, so the manifests
/// cannot be fixed at construction the way the CLI build path fixes them. This
/// reads the cell the capture writes, so acceptance always verifies the candidate
/// that was just sealed — the one about to be reported.
///
/// Reading before any capture is a hard error, not an empty default: it would
/// mean acceptance ran with nothing to verify.
pub struct HeldCandidateSource<'a> {
    captured: CapturedCandidateCell,
    /// The backend, to derive the v1 sidecar for whatever was captured. Borrowed
    /// for the hold rather than `'static`: the backend a job runs on carries
    /// that job's boot timeout (`with_boot_timeout`), so it is a per-job value
    /// and leaking one per hold would leak the job's config with it.
    backend: &'a dyn snapshot::SnapshotBackend,
    execution_id: capsule::execution_contract::ExecutionId,
}

impl<'a> HeldCandidateSource<'a> {
    pub fn new(
        captured: CapturedCandidateCell,
        backend: &'a dyn snapshot::SnapshotBackend,
        execution_id: capsule::execution_contract::ExecutionId,
    ) -> Self {
        Self {
            captured,
            backend,
            execution_id,
        }
    }

    fn receipt(&self) -> Result<snapshot::BuildReadyStateReceipt, String> {
        self.captured
            .borrow()
            .clone()
            .ok_or_else(|| "acceptance ran before any candidate was captured".to_string())
    }
}

impl snapshot::disposable_lifecycle::CandidateSource for HeldCandidateSource<'_> {
    fn legacy_manifest(&self) -> Result<snapshot::ReadyStateManifest, String> {
        Ok(self.receipt()?.manifest)
    }

    fn candidate_manifest(&self) -> Result<capsule::snapshot_manifest::SnapshotManifestV1, String> {
        let receipt = self.receipt()?;
        snapshot::disposable_lifecycle::build_v1_candidate_manifest(
            self.backend,
            self.execution_id.clone(),
            &receipt,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hold_phase::ControlFault;

    /// A lease that fails on the Nth drive, so a test can pin WHERE the capture
    /// gives up — and, with `n` large, that it drives the lease at all.
    struct CountingLease {
        drives: u32,
        fail_on: Option<u32>,
    }

    impl CountingLease {
        fn never_fails() -> Self {
            Self {
                drives: 0,
                fail_on: None,
            }
        }

        fn failing_on(n: u32) -> Self {
            Self {
                drives: 0,
                fail_on: Some(n),
            }
        }
    }

    impl LeaseKeepalive for CountingLease {
        fn keepalive(&mut self) -> Result<(), ControlFault> {
            self.drives += 1;
            if self.fail_on == Some(self.drives) {
                return Err(ControlFault {
                    message: "lease fenced".to_string(),
                });
            }
            Ok(())
        }
    }

    /// The capture renews BEFORE it pauses the guest.
    ///
    /// Everything after that point runs with no control poll in it, so a lease
    /// that is already dead must be discovered here — not after a candidate has
    /// been sealed and uploaded that nobody can report.
    #[test]
    fn a_dead_lease_is_caught_before_the_guest_is_touched() {
        // No `HeldGuest` is constructed: reaching one would need /dev/kvm, and
        // the point is that this path returns before it would.
        let mut lease = CountingLease::failing_on(1);
        let err = super::pre_capture_lease_gate(&mut lease).expect_err("must refuse");
        assert_eq!(lease.drives, 1, "exactly one drive, before any guest work");
        assert!(
            !err.source_lost,
            "a dead LEASE is not a lost GUEST: reporting it as source_lost would \
             end an attempt whose guest is still usable"
        );
        assert!(
            err.message.contains("before the capture"),
            "{}",
            err.message
        );
    }

    /// Acceptance before any capture is an error, never an empty default.
    ///
    /// Returning something benign would let the verifier run against nothing and
    /// report a verdict about an artifact that does not exist.
    #[test]
    fn reading_the_candidate_before_any_capture_fails() {
        use snapshot::disposable_lifecycle::CandidateSource;
        let cell: CapturedCandidateCell = Rc::new(RefCell::new(None));
        // A real backend, but no live VM: this path returns before any method on
        // it is reached.
        let backend = snapshot::FirecrackerBackend::new();
        let source = HeldCandidateSource::new(
            Rc::clone(&cell),
            &backend,
            capsule::execution_contract::ExecutionId::new(format!("blake3:{}", "a".repeat(64)))
                .expect("id"),
        );
        let err = source.legacy_manifest().expect_err("must refuse");
        assert!(err.contains("before any candidate"), "{err}");
    }

    /// A live lease lets the capture proceed past the gate.
    #[test]
    fn a_live_lease_passes_the_gate() {
        let mut lease = CountingLease::never_fails();
        assert!(super::pre_capture_lease_gate(&mut lease).is_ok());
        assert_eq!(lease.drives, 1);
    }

    // ── #1160 per-attempt scratch ───────────────────────────────────────────

    /// A failed attempt leaves the CAS exactly as it found it.
    ///
    /// This is where the DISK half of #1160 lives. Bounding the attempt count
    /// bounds how many failed captures can pile up; this makes each one cost
    /// nothing — a capture seals a full memory image into the job CAS before the
    /// upload it is about to fail on, guest memory differs on every capture so
    /// nothing dedupes, and no manifest will ever reference those bytes.
    ///
    /// The build's own layers are the thing that must NOT be swept: the next
    /// attempt seals against them.
    #[test]
    fn a_failed_attempt_reclaims_its_own_bytes_and_keeps_the_builds() {
        let jobdir = tempfile::tempdir().expect("tempdir");
        let store = CasStore::open(super::job_cas_root(jobdir.path())).expect("cas");

        // The build's layers, already in the CAS when the hold starts.
        let rootfs = store.put_chunk(b"rootfs layer bytes").expect("put");
        let app = store.put_chunk(b"app layer bytes").expect("put");

        // The production shape of the failure: the seal WORKS (a full memory
        // image and vmstate land in the CAS) and the upload after it does not.
        let mut sealed = None;
        let outcome: Result<(), CaptureError> =
            super::with_attempt_scratch_reclaimed(jobdir.path(), || {
                let memory = store.put_chunk(b"a whole memory image").expect("put");
                let vmstate = store.put_chunk(b"vmstate for that capture").expect("put");
                assert_eq!(store.list_chunks().expect("list").len(), 4);
                sealed = Some((memory, vmstate));
                Err(CaptureError {
                    source_lost: false,
                    message: "artifact upload failed".to_string(),
                })
            });

        assert!(outcome.is_err(), "the attempt's verdict travels unchanged");
        let (memory, vmstate) = sealed.expect("the attempt sealed");
        assert!(store.has_chunk(&rootfs), "the build's rootfs must survive");
        assert!(store.has_chunk(&app), "the build's app layer must survive");
        assert!(
            !store.has_chunk(&memory),
            "the failed attempt's memory image is unreferenced garbage"
        );
        assert!(!store.has_chunk(&vmstate));
        assert_eq!(store.list_chunks().expect("list").len(), 2);
    }

    /// A SUCCESSFUL capture's bytes are never swept.
    ///
    /// The other half of the same decision, and the one with teeth: acceptance
    /// restores the candidate from these very chunks moments later, so a sweep
    /// that ran on success would delete the artifact between sealing it and
    /// verifying it. A cleanup wired to run unconditionally fails here.
    #[test]
    fn a_successful_attempt_keeps_every_byte_it_sealed() {
        let jobdir = tempfile::tempdir().expect("tempdir");
        let store = CasStore::open(super::job_cas_root(jobdir.path())).expect("cas");
        let rootfs = store.put_chunk(b"rootfs layer bytes").expect("put");

        let mut sealed = None;
        let outcome: Result<&str, CaptureError> =
            super::with_attempt_scratch_reclaimed(jobdir.path(), || {
                sealed = Some(store.put_chunk(b"the accepted memory image").expect("put"));
                Ok("candidate")
            });

        assert_eq!(outcome.expect("ok"), "candidate");
        let candidate = sealed.expect("the attempt sealed");
        assert!(
            store.has_chunk(&candidate),
            "the candidate about to be verified must still be on disk"
        );
        assert!(store.has_chunk(&rootfs));
        assert_eq!(store.list_chunks().expect("list").len(), 2);
    }

    /// Three failed attempts cost the disk of zero candidates, not three.
    ///
    /// The measured defect was 356 captures; the attempt cap makes that three,
    /// and this is what makes three cost nothing. Each attempt re-reads
    /// residency first, so the baseline is per-attempt rather than per-hold.
    #[test]
    fn every_failed_attempt_is_reclaimed_not_just_the_first() {
        let jobdir = tempfile::tempdir().expect("tempdir");
        let store = CasStore::open(super::job_cas_root(jobdir.path())).expect("cas");
        let build = store.put_chunk(b"rootfs layer bytes").expect("put");

        for attempt in 0..crate::hold_phase::MAX_CAPTURE_ATTEMPTS {
            let mut sealed = None;
            let _: Result<(), CaptureError> =
                super::with_attempt_scratch_reclaimed(jobdir.path(), || {
                    // Distinct bytes per attempt: real guest memory never
                    // repeats, which is exactly why the CAS cannot dedupe these
                    // away by itself.
                    sealed = Some(
                        store
                            .put_chunk(format!("memory image for attempt {attempt}").as_bytes())
                            .expect("put"),
                    );
                    Err(CaptureError {
                        source_lost: false,
                        message: "upload failed".to_string(),
                    })
                });
            let sealed = sealed.expect("sealed");
            assert!(!store.has_chunk(&sealed), "attempt {attempt} left bytes");
        }

        assert!(store.has_chunk(&build));
        assert_eq!(
            store.list_chunks().expect("list").len(),
            1,
            "after a whole spent budget the CAS holds only the build's layers"
        );
    }

    /// A CAS that cannot be read disables the sweep rather than guessing.
    ///
    /// `resident_chunks` is the baseline the whole cleanup is defined against
    /// ("everything that was here before this attempt"). Without it there is no
    /// safe answer to what is garbage — a sweep against an EMPTY baseline would
    /// take the build's own layers with it and strand the hold — so the caller
    /// skips the cleanup entirely. Losing disk is recoverable; deleting the
    /// rootfs the next attempt seals against is not.
    #[test]
    fn an_unreadable_cas_yields_no_baseline_and_therefore_no_sweep() {
        let jobdir = tempfile::tempdir().expect("tempdir");
        // A FILE where `<jobdir>/cas` should be: `CasStore::open` create_dir_all's
        // into it and fails.
        std::fs::write(super::job_cas_root(jobdir.path()), b"not a directory").expect("write");
        assert!(
            super::resident_chunks(jobdir.path()).is_none(),
            "no baseline ⇒ the caller must skip the sweep"
        );
    }
}
