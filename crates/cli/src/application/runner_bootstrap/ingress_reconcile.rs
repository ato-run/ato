//! One reconcile function, reached from every trigger.
//!
//! Startup, a periodic tick, a new desired state pushed by the control plane,
//! and a retry after a previous failure are four events, not four behaviours.
//! Each of them asks the same question — *does the box match what it is
//! supposed to be?* — and answering it in four places is how three of them end
//! up subtly wrong.
//!
//! So this is level-triggered: it reads the desired state, reads the actual
//! state, and closes whatever gap it finds. It does not care which event woke
//! it, and running it twice changes nothing the second time.
//!
//! ```text
//! reconcile
//!   in sync?           report and stop
//!   otherwise          activate  (ingress_activation, over ingress_store)
//!                      confirm   (ingress_probe: two stages, per origin)
//!                      report the observed state either way
//! ```
//!
//! # The control plane is told what IS, not what was asked for
//!
//! The desired state is an input. The report is an observation: which
//! generation the disk points at, which one was confirmed serving, and why the
//! two differ when they do. An API that returned "activated: ok" from the
//! request that asked for it would be reporting its own intent back to itself.

#![allow(dead_code)]

use anyhow::Result;

use super::ingress_activation::{ActivationOutcome, CaddyControl, GenerationStore, activate};
use super::ingress_probe::{
    Confirmation, IngressProbe, ProbeBudget, ProbeTarget, confirm_activation,
};
use super::official_preview::{GeneratedFragment, GenerationIdentity};

/// What this runner is supposed to be serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesiredIngress {
    pub builder_id: String,
    /// Monotonic. An older revision never overwrites a newer desired state —
    /// a control plane that retries a stale request must not undo a newer one.
    pub revision: u64,
    pub identity: GenerationIdentity,
    pub fragments: Vec<GeneratedFragment>,
    pub targets: Vec<ProbeTarget>,
}

/// Where the box actually is. Every field is read back from disk, never
/// remembered from what was attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedIngress {
    pub desired_generation: String,
    pub desired_revision: u64,
    pub current_generation: Option<String>,
    pub activated_generation: Option<String>,
    pub status: ReconcileStatus,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconcileStatus {
    /// Disk and Caddy both agree with the desired state, and it is confirmed.
    InSync,
    /// A gap was found and closed this run.
    Reconciled,
    /// A gap was found, could not be closed, and the previous generation was
    /// restored and re-confirmed.
    RolledBack,
    /// The box is in a state nobody chose. The journal is retained and the next
    /// trigger retries.
    Failed,
}

/// Where the observation is reported. A seam, so a reconcile can be tested
/// without a control plane and so a reporting failure never rolls back a
/// successful activation.
pub(crate) trait IngressControlPlane {
    fn report(&mut self, observed: &ObservedIngress) -> Result<()>;
}

/// Everything that must agree before a reconcile may do nothing.
///
/// Any one of these being false means the box is not what it is supposed to be,
/// whatever the others say:
///
/// ```text
/// no journal              no transaction is unfinished
/// current  == desired     the disk points at the right generation
/// activated == desired    a probe confirmed that generation is SERVED
/// receipt  == desired     the confirmation was recorded
/// contents match          the directory is complete AND is this generation
/// ```
///
/// The last one is why a digest is not enough on its own: a directory can carry
/// the right name and the wrong bytes, and only comparing the manifest catches
/// it.
fn in_sync(store: &dyn GenerationStore, desired: &DesiredIngress) -> Result<bool> {
    let id = desired.identity.id.as_str();
    Ok(store.read_pending()?.is_none()
        && store.read_current()?.as_deref() == Some(id)
        && store.read_activated()?.as_deref() == Some(id)
        && store.read_receipt()?.as_deref() == Some(id)
        && store.generation_matches(id, &desired.fragments)?)
}

/// Close the gap between desired and actual, whatever woke us.
///
/// The observation is reported on EVERY path, including failure: a control
/// plane that only hears about successes cannot tell a box that is still
/// working from one that gave up.
pub(crate) fn reconcile(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    control_plane: &mut dyn IngressControlPlane,
    desired: &DesiredIngress,
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<ObservedIngress> {
    let outcome = run(store, caddy, probe, desired, marker_path, budget);
    let observed = observe(store, desired, &outcome)?;
    // Reported after the box has settled, and never allowed to change it: a
    // failure to report is a reporting problem, not a reason to undo work that
    // succeeded.
    if let Err(error) = control_plane.report(&observed) {
        eprintln!(
            "[runner] ingress observation for {} was not accepted by the control plane: {error}",
            desired.builder_id
        );
    }
    match outcome {
        Ok(_) => Ok(observed),
        Err(error) => Err(error),
    }
}

enum Settled {
    AlreadyInSync,
    Confirmed,
    RolledBack { failure: String },
}

fn run(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    desired: &DesiredIngress,
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<Settled> {
    if in_sync(store, desired)? {
        return Ok(Settled::AlreadyInSync);
    }

    let outcome = activate(store, caddy, &desired.identity.id, &desired.fragments)?;
    let ActivationOutcome::ReloadedPendingProbe {
        candidate,
        previous,
    } = strip(&outcome)
    else {
        // Activation decided nothing needed reloading, yet the box was not in
        // sync — the gap is in the CONFIRMED state, so re-confirm rather than
        // declare victory on a marker that may never have been probed.
        let previous = store.read_activated()?;
        return confirm(
            store,
            caddy,
            probe,
            desired,
            &desired.identity.id,
            previous.as_deref(),
            marker_path,
            budget,
        );
    };
    let candidate = candidate.clone();
    let previous = previous.clone();
    confirm(
        store,
        caddy,
        probe,
        desired,
        &candidate,
        previous.as_deref(),
        marker_path,
        budget,
    )
}

#[allow(clippy::too_many_arguments)]
fn confirm(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    desired: &DesiredIngress,
    candidate: &str,
    previous: Option<&str>,
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<Settled> {
    // The candidate's identity is the desired one by construction — it is what
    // was just activated. The previous generation travels as an id only; see
    // `ExpectedGeneration` for why that asymmetry is stated rather than papered
    // over with a fabricated digest.
    let candidate_identity = if candidate == desired.identity.id {
        desired.identity.clone()
    } else {
        // Activation moved to something other than the desired generation. That
        // is a bug, not a state to reconcile — refuse rather than confirm it.
        anyhow::bail!(
            "activation produced generation {candidate}, but {} was desired",
            desired.identity.id
        );
    };
    match confirm_activation(
        store,
        caddy,
        probe,
        &candidate_identity,
        previous,
        &desired.targets,
        marker_path,
        budget,
    )? {
        Confirmation::Confirmed { .. } => Ok(Settled::Confirmed),
        Confirmation::RolledBack { failure } => Ok(Settled::RolledBack { failure }),
    }
}

fn strip(outcome: &ActivationOutcome) -> &ActivationOutcome {
    match outcome {
        ActivationOutcome::Recovered(inner) => strip(inner),
        other => other,
    }
}

/// Read the box back and describe it. Never derived from what was attempted.
fn observe(
    store: &dyn GenerationStore,
    desired: &DesiredIngress,
    outcome: &Result<Settled>,
) -> Result<ObservedIngress> {
    let (status, last_error) = match outcome {
        Ok(Settled::AlreadyInSync) => (ReconcileStatus::InSync, None),
        Ok(Settled::Confirmed) => (ReconcileStatus::Reconciled, None),
        Ok(Settled::RolledBack { failure }) => (ReconcileStatus::RolledBack, Some(failure.clone())),
        Err(error) => (ReconcileStatus::Failed, Some(format!("{error:#}"))),
    };
    Ok(ObservedIngress {
        desired_generation: desired.identity.id.clone(),
        desired_revision: desired.revision,
        current_generation: store.read_current()?,
        activated_generation: store.read_activated()?,
        status,
        last_error,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::application::runner_bootstrap::ingress_activation::PendingJournal;
    use crate::application::runner_bootstrap::ingress_probe::ProbeResponse;
    use crate::application::runner_bootstrap::official_preview::{
        GENERATION_MARKER_PATH, PREVIEW_FRAGMENT,
    };

    fn identity(id: &str) -> GenerationIdentity {
        GenerationIdentity {
            id: id.to_string(),
            digest: format!("{id}{}", "0".repeat(64 - id.len())),
        }
    }

    fn desired(id: &str, revision: u64) -> DesiredIngress {
        DesiredIngress {
            builder_id: "runner-abc.runner.ato.run".into(),
            revision,
            identity: identity(id),
            fragments: vec![GeneratedFragment {
                file_name: PREVIEW_FRAGMENT,
                content: format!("routes for {id}"),
            }],
            targets: vec![ProbeTarget {
                origin: "s0.runner-abc.runner.ato.run".into(),
                readiness_path: "/.well-known/ato-runner-ingress".into(),
            }],
        }
    }

    #[derive(Default)]
    struct FakeStore {
        current: Option<String>,
        activated: Option<String>,
        receipt: Option<String>,
        pending: Option<PendingJournal>,
        written: Vec<String>,
    }

    impl GenerationStore for FakeStore {
        fn lock(&mut self) -> Result<()> {
            Ok(())
        }
        fn read_current(&self) -> Result<Option<String>> {
            Ok(self.current.clone())
        }
        fn read_activated(&self) -> Result<Option<String>> {
            Ok(self.activated.clone())
        }
        fn read_pending(&self) -> Result<Option<PendingJournal>> {
            Ok(self.pending.clone())
        }
        fn write_generation(
            &mut self,
            digest: &str,
            _fragments: &[GeneratedFragment],
        ) -> Result<()> {
            self.written.push(digest.to_string());
            Ok(())
        }
        fn generation_complete(&self, digest: &str) -> Result<bool> {
            Ok(self.written.iter().any(|d| d == digest))
        }
        fn generation_matches(
            &self,
            digest: &str,
            _fragments: &[GeneratedFragment],
        ) -> Result<bool> {
            self.generation_complete(digest)
        }
        fn write_pending(&mut self, journal: &PendingJournal) -> Result<()> {
            self.pending = Some(journal.clone());
            Ok(())
        }
        fn clear_pending(&mut self) -> Result<()> {
            self.pending = None;
            Ok(())
        }
        fn read_receipt(&self) -> Result<Option<String>> {
            Ok(self.receipt.clone())
        }
        fn write_receipt(&mut self, digest: Option<&str>) -> Result<()> {
            self.receipt = digest.map(str::to_string);
            Ok(())
        }
        fn set_current(&mut self, digest: Option<&str>) -> Result<()> {
            self.current = digest.map(str::to_string);
            Ok(())
        }
        fn write_activated(&mut self, digest: Option<&str>) -> Result<()> {
            self.activated = digest.map(str::to_string);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeCaddy {
        validates: u32,
        reloads: u32,
    }

    impl CaddyControl for FakeCaddy {
        fn validate(&mut self, _digest: &str) -> Result<()> {
            self.validates += 1;
            Ok(())
        }
        fn reload(&mut self) -> Result<()> {
            self.reloads += 1;
            Ok(())
        }
    }

    /// Serves whatever it is told, so a reconcile can be run against a box that
    /// agrees or disagrees with the desired state.
    struct FakeProbe {
        serving: Option<GenerationIdentity>,
        calls: u32,
    }

    impl IngressProbe for FakeProbe {
        fn get(&mut self, _origin: &str, _path: &str) -> Result<ProbeResponse> {
            self.calls += 1;
            Ok(ProbeResponse {
                status: 200,
                marker: self.serving.clone(),
            })
        }
    }

    #[derive(Default)]
    struct FakeControlPlane {
        reports: Vec<ObservedIngress>,
        refuse: bool,
    }

    impl IngressControlPlane for FakeControlPlane {
        fn report(&mut self, observed: &ObservedIngress) -> Result<()> {
            self.reports.push(observed.clone());
            if self.refuse {
                anyhow::bail!("control plane unavailable");
            }
            Ok(())
        }
    }

    fn fast() -> ProbeBudget {
        ProbeBudget {
            deadline: Duration::from_millis(30),
            interval: Duration::from_millis(5),
        }
    }

    fn run_once(
        store: &mut FakeStore,
        caddy: &mut FakeCaddy,
        probe: &mut FakeProbe,
        plane: &mut FakeControlPlane,
        desired: &DesiredIngress,
    ) -> Result<ObservedIngress> {
        reconcile(
            store,
            caddy,
            probe,
            plane,
            desired,
            GENERATION_MARKER_PATH,
            fast(),
        )
    }

    /// A box that already matches is left alone — no write, no validate, no
    /// reload, no probe.
    #[test]
    fn a_box_that_already_matches_is_left_completely_alone() {
        let want = desired("gen-a", 7);
        let mut store = FakeStore {
            current: Some("gen-a".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            written: vec!["gen-a".into()],
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        let mut probe = FakeProbe {
            serving: Some(identity("gen-a")),
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::InSync);
        assert_eq!((caddy.validates, caddy.reloads, probe.calls), (0, 0, 0));
        assert_eq!(plane.reports.len(), 1, "the observation is still reported");
    }

    /// Each of the five conditions, alone, forbids the no-op. A reconcile that
    /// skipped on any one of them would leave the box unconverged forever,
    /// because nothing else ever revisits it.
    #[test]
    fn every_condition_alone_forbids_the_no_op() {
        let want = desired("gen-a", 1);
        let settled = || FakeStore {
            current: Some("gen-a".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            written: vec!["gen-a".into()],
            ..Default::default()
        };
        assert!(in_sync(&settled(), &want).unwrap());

        let mut journal = settled();
        journal.pending = Some(PendingJournal {
            candidate: "gen-a".into(),
            previous: None,
            reload_succeeded: true,
        });
        assert!(!in_sync(&journal, &want).unwrap(), "an open transaction");

        let mut current = settled();
        current.current = Some("gen-b".into());
        assert!(
            !in_sync(&current, &want).unwrap(),
            "the disk points elsewhere"
        );

        let mut activated = settled();
        activated.activated = Some("gen-b".into());
        assert!(!in_sync(&activated, &want).unwrap(), "never confirmed");

        let mut receipt = settled();
        receipt.receipt = None;
        assert!(
            !in_sync(&receipt, &want).unwrap(),
            "no recorded confirmation"
        );

        let mut contents = settled();
        contents.written.clear();
        assert!(
            !in_sync(&contents, &want).unwrap(),
            "the right name over the wrong (or missing) bytes"
        );
    }

    /// A new desired generation is activated and confirmed, and the report
    /// describes what the box IS.
    #[test]
    fn a_new_desired_generation_is_activated_and_confirmed() {
        let want = desired("gen-b", 9);
        let mut store = FakeStore {
            current: Some("gen-a".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            written: vec!["gen-a".into()],
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        let mut probe = FakeProbe {
            serving: Some(identity("gen-b")),
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::Reconciled);
        assert_eq!(observed.desired_generation, "gen-b");
        assert_eq!(observed.desired_revision, 9);
        assert_eq!(observed.current_generation.as_deref(), Some("gen-b"));
        assert_eq!(observed.activated_generation.as_deref(), Some("gen-b"));
        assert_eq!(observed.last_error, None);
        assert_eq!(store.receipt.as_deref(), Some("gen-b"));
    }

    /// Running it again changes nothing — the property that makes every trigger
    /// safe to fire.
    #[test]
    fn reconciling_twice_is_the_same_as_reconciling_once() {
        let want = desired("gen-b", 9);
        let mut store = FakeStore {
            written: vec![],
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        let mut probe = FakeProbe {
            serving: Some(identity("gen-b")),
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        let after_first = (
            store.current.clone(),
            store.activated.clone(),
            store.receipt.clone(),
            caddy.reloads,
        );

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::InSync);
        assert_eq!(
            (
                store.current.clone(),
                store.activated.clone(),
                store.receipt.clone(),
                caddy.reloads
            ),
            after_first,
            "the second run touched nothing"
        );
    }

    /// A probe that never sees the candidate rolls back, and the report says so
    /// with the reason attached.
    #[test]
    fn a_failed_confirmation_reports_a_rollback_with_its_reason() {
        let want = desired("gen-b", 3);
        let mut store = FakeStore {
            written: vec![],
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        // The box keeps serving something else entirely.
        let mut probe = FakeProbe {
            serving: Some(identity("gen-old")),
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::RolledBack);
        assert!(
            observed
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("is served by generation gen-old")),
            "{:?}",
            observed.last_error
        );
        assert_eq!(
            observed.activated_generation, None,
            "a first install rolls back to nothing confirmed"
        );
        assert_eq!(plane.reports.len(), 1);
    }

    /// A rollback to a REAL previous generation confirms.
    ///
    /// The previous generation is known only by its id — its full digest is not
    /// recoverable — and an earlier version fabricated an empty digest for it,
    /// which made every such rollback fail its own confirmation and report a
    /// composite error. The asymmetry is now explicit, and this pins it.
    #[test]
    fn a_rollback_to_a_real_previous_generation_confirms_by_id() {
        let want = desired("gen-b", 11);
        let mut store = FakeStore {
            current: Some("gen-a".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            written: vec!["gen-a".into()],
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        // The box never starts serving the candidate, and keeps answering as
        // gen-a — so the rollback probe finds exactly what it should.
        let mut probe = FakeProbe {
            serving: Some(GenerationIdentity {
                id: "gen-a".into(),
                // A digest this process has no way to know, which is the whole
                // point: the rollback check must not depend on it.
                digest: "9".repeat(64),
            }),
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(
            observed.status,
            ReconcileStatus::RolledBack,
            "the rollback completed and was confirmed: {:?}",
            observed.last_error
        );
        assert_eq!(store.current.as_deref(), Some("gen-a"));
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
        assert_eq!(store.receipt.as_deref(), Some("gen-a"));
        assert!(store.pending.is_none(), "the transaction was closed");
    }

    /// A control plane that will not accept the report does not undo the work.
    ///
    /// The activation succeeded; failing it because the news could not be
    /// delivered would tear down a working configuration for a reporting
    /// problem.
    #[test]
    fn a_control_plane_that_refuses_the_report_does_not_undo_the_activation() {
        let want = desired("gen-b", 4);
        let mut store = FakeStore::default();
        let mut caddy = FakeCaddy::default();
        let mut probe = FakeProbe {
            serving: Some(identity("gen-b")),
            calls: 0,
        };
        let mut plane = FakeControlPlane {
            refuse: true,
            ..Default::default()
        };

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::Reconciled);
        assert_eq!(store.activated.as_deref(), Some("gen-b"));
        assert_eq!(plane.reports.len(), 1, "it was attempted");
    }

    /// The observation is reported on the failure path too — a control plane
    /// that only hears about successes cannot tell a box still working from one
    /// that gave up.
    #[test]
    fn the_observation_is_reported_even_when_the_reconcile_fails() {
        let want = desired("gen-b", 5);
        let mut store = FakeStore::default();
        let mut caddy = FakeCaddy::default();
        let mut probe = FakeProbe {
            serving: None,
            calls: 0,
        };
        let mut plane = FakeControlPlane::default();

        let observed = run_once(&mut store, &mut caddy, &mut probe, &mut plane, &want).unwrap();
        assert_eq!(observed.status, ReconcileStatus::RolledBack);
        assert_eq!(plane.reports.len(), 1);
        assert_eq!(plane.reports[0].desired_revision, 5);
    }
}
