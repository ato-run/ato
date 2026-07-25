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

use capsule::snapshot_manifest::SnapshotManifestV1;
use capsulefs::CasStore;

use crate::acceptance::{
    AcceptanceBudget, CandidateSnapshot, DisposableAcceptanceLifecycle, DisposableSessionHandle,
    VerificationOutcome, sanitize_untrusted_environment,
};
use crate::manifest::ReadyStateManifest;
use crate::{RestoreReadyStateInput, RestoredSession, SnapshotBackend};

/// Real (non-stubbed) [`DisposableAcceptanceLifecycle`] backed by an actual
/// [`SnapshotBackend`]: capture wraps the already-sealed candidate (see
/// [`default_acceptance_config`]'s doc for why there is one attempt), create
/// allocates the disposable overlay, restore calls the REAL
/// `backend.restore`, and destroy calls the REAL `backend.stop` — no phase is
/// faked or self-attesting.
pub struct BackendDisposableLifecycle<'a> {
    pub backend: &'a dyn SnapshotBackend,
    pub store: &'a CasStore,
    pub legacy_manifest: ReadyStateManifest,
    pub candidate_manifest: SnapshotManifestV1,
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

impl DisposableAcceptanceLifecycle for BackendDisposableLifecycle<'_> {
    fn capture_candidate(
        &mut self,
        _attempt: u32,
        _budget: &AcceptanceBudget,
    ) -> Result<CandidateSnapshot, String> {
        self.last_candidate = Some(self.candidate_manifest.clone());
        Ok(CandidateSnapshot {
            manifest: self.candidate_manifest.clone(),
        })
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
                manifest: self.legacy_manifest.clone(),
                overlay_root: overlay,
                host_runner_class: None,
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
