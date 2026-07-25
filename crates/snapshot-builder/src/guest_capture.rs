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

use std::path::PathBuf;

use snapshot::{HeldCaptureFailure, HeldGuest};

use crate::hold_phase::{CaptureAction, CaptureError, HeldCapture, LeaseKeepalive};

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
}

/// These have no caller yet: the job-loop wiring that boots the hold and fronts
/// it with an ingress is the next slice, and the lane stays OFF until then
/// (`interactive_capture` is not advertised on the claim).
#[allow(dead_code)]
impl<'a> GuestCaptureAction<'a> {
    pub fn new(guest: HeldGuest<'a>, ctx: CaptureContext) -> Self {
        Self { guest, ctx }
    }

    /// The address a host-side proxy fronts to reach the held workload.
    pub fn workload_addr(&self) -> String {
        self.guest.workload_addr()
    }

    /// Tear the hold down. Consumes the action, so no capture can follow.
    pub fn release(self) {
        self.guest.release();
    }
}

/// Fail a capture while preserving the ADR-012 bit.
fn capture_failed(source_lost: bool, message: String) -> CaptureError {
    CaptureError {
        source_lost,
        message,
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
    fn capture(
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

    /// A live lease lets the capture proceed past the gate.
    #[test]
    fn a_live_lease_passes_the_gate() {
        let mut lease = CountingLease::never_fails();
        assert!(super::pre_capture_lease_gate(&mut lease).is_ok());
        assert_eq!(lease.drives, 1);
    }
}
