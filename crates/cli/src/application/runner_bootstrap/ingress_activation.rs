//! Activating a runner-ingress generation, transactionally.
//!
//! A generation is a set of Caddy fragments rendered together
//! ([`super::official_preview::render_generation`]). Activating one means: put
//! it on disk, prove Caddy accepts it, point `current` at it, and make Caddy
//! actually run it. Those are four different places a crash can land, and a
//! naive "swap the symlink, then reload" cannot tell the dangerous one apart
//! from success:
//!
//! ```text
//! swap current -> new generation
//! (process dies)
//! reload never ran
//! ```
//!
//! Next run, a check that only compares `current` against the desired digest
//! concludes "no change" — while Caddy is still serving the OLD config and the
//! disk says otherwise. Nothing ever reconciles them, and the divergence is
//! invisible until an origin misbehaves.
//!
//! So three pieces of state are kept apart, because they answer three different
//! questions:
//!
//! ```text
//! current               which generation the DISK wants to be running
//! activated-generation  which generation Caddy was last CONFIRMED to run
//! activation.pending    a transaction is in flight (and what to undo to)
//! ```
//!
//! `current != activated`, or a pending journal, means the two disagree — and
//! that is never a no-op, whatever the digests say.
//!
//! # What a reload does and does not prove
//!
//! A successful `caddy reload` proves the configuration was ACCEPTED and the
//! switch was requested. It does not prove the new generation is being served:
//! the origin could still answer from the old routes, or from another vhost
//! entirely. So this slice deliberately stops short of confirming anything.
//!
//! ```text
//! at the end of a successful activation here:
//!   current              = candidate
//!   activated-generation = previous   (UNCHANGED)
//!   activation.pending   = candidate + previous + reload_succeeded
//! ```
//!
//! `activated-generation` means "a two-stage probe confirmed this generation is
//! actually being served", and only the probe stage may set it. Anything else
//! would make the receipt a record of a request rather than of an outcome.
//!
//! # Seams
//!
//! Everything that touches the host goes through [`GenerationStore`] and
//! [`CaddyControl`], so every failure ordering below is exercised by fault
//! injection rather than by hoping. External commands run as argv, never as a
//! shell string.

#![allow(dead_code)]

use anyhow::{Result, bail};

use super::official_preview::GeneratedFragment;

/// The in-flight record written before `current` moves and removed only after
/// the outcome is settled. `previous` is what a rollback restores — including
/// `None`, which is a real value on a first install and must not be confused
/// with "unknown".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingJournal {
    pub candidate: String,
    pub previous: Option<String>,
    /// Whether `caddy reload` returned success for `candidate`. It is the
    /// difference between "roll this back" and "this still needs probing":
    /// without a successful reload there is nothing to confirm, and with one
    /// the candidate may already be live.
    pub reload_succeeded: bool,
}

/// What an activation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivationOutcome {
    /// Disk, Caddy and the confirmed receipt already agree on this generation.
    NoOp,
    /// The candidate is on disk, validated, current, and Caddy accepted it —
    /// but nothing has yet confirmed it is being SERVED. The transaction stays
    /// open until the probe stage confirms or rolls it back.
    ReloadedPendingProbe {
        candidate: String,
        previous: Option<String>,
    },
    /// An interrupted transaction was finished or undone before this call could
    /// proceed; the recovery outcome is carried so a caller can log it.
    Recovered(Box<ActivationOutcome>),
}

/// Host filesystem operations, as a seam.
pub(crate) trait GenerationStore {
    /// Serialize concurrent setups. Held for the whole activation.
    fn lock(&mut self) -> Result<()>;
    /// The digest `current` points at, or `None` when it does not exist.
    fn read_current(&self) -> Result<Option<String>>;
    /// The last generation Caddy was CONFIRMED to be running.
    fn read_activated(&self) -> Result<Option<String>>;
    fn read_pending(&self) -> Result<Option<PendingJournal>>;
    /// Materialize a generation directory and fsync it. Idempotent.
    fn write_generation(&mut self, digest: &str, fragments: &[GeneratedFragment]) -> Result<()>;
    /// Whether every fragment of `digest` is present and complete on disk. A
    /// half-written generation must never become `current`.
    fn generation_complete(&self, digest: &str) -> Result<bool>;
    fn write_pending(&mut self, journal: &PendingJournal) -> Result<()>;
    fn clear_pending(&mut self) -> Result<()>;
    /// The activation receipt for the last CONFIRMED generation, if any. A
    /// separate artifact from the activated marker so the crash between the two
    /// is recoverable rather than ambiguous.
    fn read_receipt(&self) -> Result<Option<String>>;
    fn write_receipt(&mut self, digest: Option<&str>) -> Result<()>;
    /// Atomically point `current` at `digest`, or remove it for `None`.
    fn set_current(&mut self, digest: Option<&str>) -> Result<()>;
    fn write_activated(&mut self, digest: Option<&str>) -> Result<()>;
}

/// Caddy operations, as a seam. `validate` checks a COMPLETE configuration that
/// references the candidate — validating the fragment alone would miss exactly
/// the errors that only appear in composition (a duplicate site block).
pub(crate) trait CaddyControl {
    fn validate(&mut self, digest: &str) -> Result<()>;
    fn reload(&mut self) -> Result<()>;
}

/// Both the failure that started a rollback and the failure of the rollback
/// itself. Never collapsed into one: the first says what the operator wanted
/// and could not have, the second says the box is now in a state nobody chose.
#[derive(Debug)]
pub(crate) struct ActivationFailure {
    pub primary: String,
    pub rollback: Option<String>,
}

impl std::fmt::Display for ActivationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rollback {
            None => write!(f, "activation failed: {}", self.primary),
            Some(rollback) => write!(
                f,
                "activation failed: {} — AND the rollback also failed: {rollback}. \
                 Caddy is running neither the intended nor the previous configuration; \
                 the pending journal is left in place so the next run retries the recovery.",
                self.primary
            ),
        }
    }
}

impl std::error::Error for ActivationFailure {}

/// Finish or undo an interrupted transaction, then activate `digest`.
///
/// Recovery runs FIRST and unconditionally. A caller that skipped it because
/// "the digest matches anyway" would leave a swapped-but-never-reloaded
/// generation swapped forever.
pub(crate) fn activate(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    digest: &str,
    fragments: &[GeneratedFragment],
) -> Result<ActivationOutcome> {
    store.lock()?;
    let recovered = recover(store, caddy)?;

    // A no-op requires everything to agree: the disk points here, a probe
    // CONFIRMED this generation is served, a receipt records it, no transaction
    // is open, and the directory is complete. Dropping any one of those is how a
    // half-written, never-reloaded or never-confirmed generation gets reported
    // as "already active".
    if store.read_pending()?.is_none()
        && store.read_current()?.as_deref() == Some(digest)
        && store.read_activated()?.as_deref() == Some(digest)
        && store.read_receipt()?.as_deref() == Some(digest)
        && store.generation_complete(digest)?
    {
        return Ok(match recovered {
            Some(outcome) => ActivationOutcome::Recovered(Box::new(outcome)),
            None => ActivationOutcome::NoOp,
        });
    }

    store.write_generation(digest, fragments)?;
    if !store.generation_complete(digest)? {
        bail!("generation {digest} is incomplete after being written — refusing to activate it");
    }

    // Validate BEFORE anything the box is serving can move. A validation
    // failure must leave `current` and the activated receipt exactly as they
    // were, which is only true while neither has been touched.
    caddy.validate(digest).map_err(|error| ActivationFailure {
        primary: format!("caddy validate rejected generation {digest}: {error}"),
        rollback: None,
    })?;

    let previous = store.read_current()?;
    let journal = PendingJournal {
        candidate: digest.to_string(),
        previous: previous.clone(),
        reload_succeeded: false,
    };
    store.write_pending(&journal)?;
    store.set_current(Some(digest))?;

    match caddy.reload() {
        Ok(()) => {
            // The transaction stays OPEN. A reload proves acceptance, not
            // service — `activated-generation` and the receipt belong to the
            // probe stage, and writing either here would record a request as an
            // outcome. What is recorded is the one new fact: the reload landed.
            store.write_pending(&PendingJournal {
                reload_succeeded: true,
                ..journal
            })?;
            let outcome = ActivationOutcome::ReloadedPendingProbe {
                candidate: digest.to_string(),
                previous,
            };
            Ok(match recovered {
                Some(recovery) => ActivationOutcome::Recovered(Box::new(recovery)),
                None => outcome,
            })
        }
        Err(error) => Err(roll_back(store, caddy, previous, error.to_string()).into()),
    }
}

/// Put `current` back and make Caddy run it again.
///
/// `previous == None` is a first install: the correct restore is to remove
/// `current`, not to leave it pointing at a generation the operator never
/// confirmed.
fn roll_back(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    previous: Option<String>,
    primary: String,
) -> ActivationFailure {
    let attempt = (|| -> Result<()> {
        store.set_current(previous.as_deref())?;
        if previous.is_some() {
            caddy.reload()?;
        } else {
            // Nothing to reload INTO. The candidate is no longer referenced, so
            // a reload here would be asking Caddy to run a configuration with
            // no generation at all; leaving it on its current in-memory config
            // is the safer end state, and the receipt below records the truth.
        }
        store.write_activated(previous.as_deref())?;
        store.write_receipt(previous.as_deref())?;
        store.clear_pending()?;
        Ok(())
    })();

    match attempt {
        Ok(()) => ActivationFailure {
            primary,
            rollback: None,
        },
        Err(error) => ActivationFailure {
            primary,
            // The journal is deliberately NOT cleared: the next run must see
            // that a transaction is unfinished.
            rollback: Some(error.to_string()),
        },
    }
}

/// Reconcile disk, Caddy and the confirmed state at entry. `None` means there
/// was nothing to do.
///
/// A no-op is forbidden by EITHER disagreement alone — a pending journal, or
/// `current != activated`. A digest match is never sufficient on its own: the
/// journal is the only witness to a transaction that got far enough to move
/// `current` but not far enough to be confirmed.
///
/// The three crash points, and what each resolves to:
///
/// ```text
/// activated not updated        roll back to previous, OR (reload landed)
///                              leave the candidate for the probe stage
/// activated updated, no receipt regenerate the receipt and finalize
/// receipt present, journal too  delete the journal and finalize
/// ```
pub(crate) fn recover(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
) -> Result<Option<ActivationOutcome>> {
    let pending = store.read_pending()?;
    let current = store.read_current()?;
    let activated = store.read_activated()?;
    let receipt = store.read_receipt()?;
    if pending.is_none() && current == activated && receipt == activated {
        return Ok(None);
    }

    let Some(journal) = pending else {
        // No transaction, but the confirmed state disagrees with itself. The
        // activated marker is the authority (a probe set it); the receipt is a
        // derived record, so it is rewritten rather than trusted.
        store.write_receipt(activated.as_deref())?;
        return Ok(Some(ActivationOutcome::Recovered(Box::new(
            ActivationOutcome::NoOp,
        ))));
    };

    // Case 3: the receipt already records this candidate — the transaction
    // succeeded and only the journal outlived it.
    if receipt.as_deref() == Some(journal.candidate.as_str()) {
        store.clear_pending()?;
        return Ok(Some(ActivationOutcome::Recovered(Box::new(
            ActivationOutcome::NoOp,
        ))));
    }

    // Case 2: a probe already confirmed the candidate; only the receipt is
    // missing. Regenerate it rather than re-running an activation.
    if activated.as_deref() == Some(journal.candidate.as_str()) {
        store.write_receipt(Some(&journal.candidate))?;
        store.clear_pending()?;
        return Ok(Some(ActivationOutcome::Recovered(Box::new(
            ActivationOutcome::NoOp,
        ))));
    }

    // Case 1: nothing confirmed the candidate.
    //
    // An incomplete or missing generation is never activated, whatever
    // `current` says — that is precisely what a crash mid-write leaves behind.
    let usable = match current.as_deref() {
        Some(digest) => digest == journal.candidate && store.generation_complete(digest)?,
        None => false,
    };
    if !usable {
        let failure = roll_back(
            store,
            caddy,
            journal.previous.clone(),
            format!(
                "generation {} is missing or incomplete",
                current.as_deref().unwrap_or("<none>")
            ),
        );
        if failure.rollback.is_some() {
            return Err(failure.into());
        }
        return Ok(Some(ActivationOutcome::Recovered(Box::new(
            ActivationOutcome::NoOp,
        ))));
    }

    if journal.reload_succeeded {
        // The candidate may already be live. Confirming or rolling it back is
        // the probe stage's decision, and guessing either way here would be the
        // same mistake as treating a reload as a confirmation. Hand it on with
        // the transaction still open.
        return Ok(Some(ActivationOutcome::ReloadedPendingProbe {
            candidate: journal.candidate.clone(),
            previous: journal.previous.clone(),
        }));
    }

    // The reload never landed: re-offer the candidate once, then fall back.
    match caddy
        .validate(&journal.candidate)
        .and_then(|()| caddy.reload())
    {
        Ok(()) => {
            store.write_pending(&PendingJournal {
                reload_succeeded: true,
                ..journal.clone()
            })?;
            Ok(Some(ActivationOutcome::ReloadedPendingProbe {
                candidate: journal.candidate.clone(),
                previous: journal.previous.clone(),
            }))
        }
        Err(error) => {
            let failure = roll_back(store, caddy, journal.previous.clone(), error.to_string());
            Err(failure.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::application::runner_bootstrap::official_preview::{
        PREVIEW_FRAGMENT, WIZARD_FRAGMENT,
    };

    /// An in-memory store that can be interrupted at any step, so the orderings
    /// below are exercised rather than assumed.
    #[derive(Default)]
    struct FakeStore {
        current: Option<String>,
        activated: Option<String>,
        pending: Option<PendingJournal>,
        receipt: Option<String>,
        /// digest -> whether the directory is complete
        generations: BTreeMap<String, bool>,
        locks: u32,
        /// The step name that should fail, once.
        fail_step: Option<&'static str>,
        steps: Vec<String>,
    }

    impl FakeStore {
        fn gate(&mut self, step: &'static str) -> Result<()> {
            self.steps.push(step.to_string());
            if self.fail_step == Some(step) {
                self.fail_step = None;
                bail!("injected failure at {step}");
            }
            Ok(())
        }
    }

    impl GenerationStore for FakeStore {
        fn lock(&mut self) -> Result<()> {
            self.locks += 1;
            self.gate("lock")
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
            self.gate("write_generation")?;
            self.generations.insert(digest.to_string(), true);
            Ok(())
        }
        fn generation_complete(&self, digest: &str) -> Result<bool> {
            Ok(*self.generations.get(digest).unwrap_or(&false))
        }
        fn write_pending(&mut self, journal: &PendingJournal) -> Result<()> {
            self.gate("write_pending")?;
            self.pending = Some(journal.clone());
            Ok(())
        }
        fn clear_pending(&mut self) -> Result<()> {
            self.gate("clear_pending")?;
            self.pending = None;
            Ok(())
        }
        fn read_receipt(&self) -> Result<Option<String>> {
            Ok(self.receipt.clone())
        }
        fn write_receipt(&mut self, digest: Option<&str>) -> Result<()> {
            self.gate("write_receipt")?;
            self.receipt = digest.map(str::to_string);
            Ok(())
        }
        fn set_current(&mut self, digest: Option<&str>) -> Result<()> {
            self.gate("set_current")?;
            self.current = digest.map(str::to_string);
            Ok(())
        }
        fn write_activated(&mut self, digest: Option<&str>) -> Result<()> {
            self.gate("write_activated")?;
            self.activated = digest.map(str::to_string);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeCaddy {
        validates: u32,
        reloads: u32,
        fail_validate: bool,
        /// Fail the Nth reload (1-based); later reloads succeed.
        fail_reload_on: Option<u32>,
        fail_all_reloads: bool,
    }

    impl CaddyControl for FakeCaddy {
        fn validate(&mut self, _digest: &str) -> Result<()> {
            self.validates += 1;
            if self.fail_validate {
                bail!("injected validate failure");
            }
            Ok(())
        }
        fn reload(&mut self) -> Result<()> {
            self.reloads += 1;
            if self.fail_all_reloads || self.fail_reload_on == Some(self.reloads) {
                bail!("injected reload failure");
            }
            Ok(())
        }
    }

    fn fragments() -> Vec<GeneratedFragment> {
        vec![
            GeneratedFragment {
                file_name: PREVIEW_FRAGMENT,
                content: "preview".into(),
            },
            GeneratedFragment {
                file_name: WIZARD_FRAGMENT,
                content: "wizard".into(),
            },
        ]
    }

    /// A box where a probe already confirmed `digest`: disk, activated marker
    /// and receipt all agree, and no transaction is open.
    fn settled(digest: &str) -> FakeStore {
        FakeStore {
            current: Some(digest.into()),
            activated: Some(digest.into()),
            receipt: Some(digest.into()),
            generations: BTreeMap::from([(digest.to_string(), true)]),
            ..Default::default()
        }
    }

    fn journal(candidate: &str, previous: Option<&str>, reloaded: bool) -> PendingJournal {
        PendingJournal {
            candidate: candidate.into(),
            previous: previous.map(str::to_string),
            reload_succeeded: reloaded,
        }
    }

    /// (1) An unchanged re-run validates nothing, swaps nothing, reloads
    /// nothing.
    ///
    /// Without this, every `ato runner setup` on an unchanged box bounces the
    /// live config for no reason.
    #[test]
    fn an_unchanged_rerun_does_not_validate_swap_or_reload() {
        let mut store = settled("gen-a");
        let mut caddy = FakeCaddy::default();
        let outcome = activate(&mut store, &mut caddy, "gen-a", &fragments()).expect("no-op");
        assert_eq!(outcome, ActivationOutcome::NoOp);
        assert_eq!((caddy.validates, caddy.reloads), (0, 0));
        assert!(!store.steps.iter().any(|s| s == "set_current"));
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
    }

    /// (2) A validation failure leaves `current` and the activated receipt
    /// exactly as they were — the box keeps serving what it was serving.
    #[test]
    fn a_validation_failure_changes_neither_current_nor_the_receipt() {
        let mut store = settled("gen-a");
        let mut caddy = FakeCaddy {
            fail_validate: true,
            ..Default::default()
        };
        let error = activate(&mut store, &mut caddy, "gen-b", &fragments()).expect_err("refused");
        assert!(format!("{error:#}").contains("caddy validate rejected"));
        assert_eq!(store.current.as_deref(), Some("gen-a"));
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
        assert!(store.pending.is_none(), "no transaction was ever opened");
        assert_eq!(caddy.reloads, 0);
    }

    /// (3) Interrupted after the swap, before the reload: the next run must NOT
    /// see a no-op.
    ///
    /// This is the crash the three-state model exists for. `current` already
    /// says `gen-b`; only the receipt reveals that Caddy never got it.
    #[test]
    fn an_interruption_between_swap_and_reload_is_recovered_next_run() {
        let mut store = FakeStore {
            current: Some("gen-b".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            pending: Some(journal("gen-b", Some("gen-a"), false)),
            generations: BTreeMap::from([("gen-a".into(), true), ("gen-b".into(), true)]),
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();

        let outcome = activate(&mut store, &mut caddy, "gen-b", &fragments()).expect("recovers");
        assert!(
            matches!(outcome, ActivationOutcome::Recovered(_)),
            "{outcome:?}"
        );
        assert!(
            caddy.reloads >= 1,
            "the pending generation must be reloaded"
        );
        assert_eq!(
            store.activated.as_deref(),
            Some("gen-a"),
            "a reload confirms nothing — only a probe may move the activated marker"
        );
        assert!(
            store.pending.as_ref().is_some_and(|j| j.reload_succeeded),
            "the transaction stays open, now recording that the reload landed"
        );
    }

    /// (4) Interrupted after a SUCCESSFUL reload but before the receipt: safe to
    /// simply reload again.
    ///
    /// Reloading a config Caddy already runs is harmless. Believing it runs one
    /// it does not is the failure being prevented, so the tie is broken toward
    /// the redundant reload.
    #[test]
    fn an_interruption_after_the_activated_marker_before_the_receipt_is_finalized() {
        // A probe already confirmed gen-b and set the marker; the crash landed
        // before the receipt. Regenerating the receipt is the whole fix — no
        // reload, no re-probe.
        let mut store = FakeStore {
            current: Some("gen-b".into()),
            activated: Some("gen-b".into()),
            receipt: Some("gen-a".into()),
            pending: Some(journal("gen-b", Some("gen-a"), true)),
            generations: BTreeMap::from([("gen-a".into(), true), ("gen-b".into(), true)]),
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        recover(&mut store, &mut caddy).expect("recovers");
        assert_eq!(store.receipt.as_deref(), Some("gen-b"));
        assert!(store.pending.is_none());
        assert_eq!(caddy.reloads, 0, "nothing needed reloading");
    }

    /// A reload that landed leaves the candidate for the PROBE stage, not for a
    /// guess in either direction.
    #[test]
    fn a_reloaded_but_unconfirmed_candidate_is_handed_to_the_probe_stage() {
        let mut store = FakeStore {
            current: Some("gen-b".into()),
            activated: Some("gen-a".into()),
            receipt: Some("gen-a".into()),
            pending: Some(journal("gen-b", Some("gen-a"), true)),
            generations: BTreeMap::from([("gen-a".into(), true), ("gen-b".into(), true)]),
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();
        let outcome = recover(&mut store, &mut caddy).expect("recovers");
        assert_eq!(
            outcome,
            Some(ActivationOutcome::ReloadedPendingProbe {
                candidate: "gen-b".into(),
                previous: Some("gen-a".into()),
            })
        );
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
        assert!(store.pending.is_some(), "the transaction is still open");
        assert_eq!(caddy.reloads, 0, "it was already reloaded");
    }

    /// A journal that outlived its own transaction still forbids a no-op.
    ///
    /// The crash here is narrow — receipt written, journal not yet cleared — so
    /// `current == activated` and every digest agrees. Only the journal says a
    /// transaction was in flight, and if that alone did not block the no-op the
    /// record would sit there forever, making every future run look like a
    /// recovery that never happens.
    #[test]
    fn a_journal_that_outlived_its_transaction_still_forbids_a_no_op() {
        let mut store = settled("gen-b");
        store.pending = Some(journal("gen-b", Some("gen-a"), true));
        let mut caddy = FakeCaddy::default();

        let outcome = activate(&mut store, &mut caddy, "gen-b", &fragments()).expect("recovers");
        assert!(
            matches!(outcome, ActivationOutcome::Recovered(_)),
            "{outcome:?}"
        );
        assert!(
            store.pending.is_none(),
            "the stale journal must be cleared, or it blocks every later run"
        );
        assert_eq!(store.activated.as_deref(), Some("gen-b"));
    }

    /// (5) A reload failure restores the old symlink and reloads the old
    /// configuration.
    #[test]
    fn a_reload_failure_restores_and_reloads_the_previous_generation() {
        let mut store = settled("gen-a");
        store.generations.insert("gen-b".into(), true);
        let mut caddy = FakeCaddy {
            fail_reload_on: Some(1),
            ..Default::default()
        };

        let error = activate(&mut store, &mut caddy, "gen-b", &fragments()).expect_err("fails");
        let message = format!("{error:#}");
        assert!(message.contains("activation failed"), "{message}");
        assert!(
            !message.contains("rollback also failed"),
            "the rollback succeeded: {message}"
        );
        assert_eq!(store.current.as_deref(), Some("gen-a"));
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
        assert!(store.pending.is_none());
        assert_eq!(caddy.reloads, 2, "the failed one, then the restore");
    }

    /// (6) When the rollback reload ALSO fails, both errors are reported and the
    /// journal is left behind for the next run.
    #[test]
    fn a_failed_rollback_reports_both_errors_and_keeps_the_journal() {
        let mut store = settled("gen-a");
        store.generations.insert("gen-b".into(), true);
        let mut caddy = FakeCaddy {
            fail_all_reloads: true,
            ..Default::default()
        };

        let error = activate(&mut store, &mut caddy, "gen-b", &fragments()).expect_err("fails");
        let message = format!("{error:#}");
        assert!(message.contains("rollback also failed"), "{message}");
        assert!(
            store.pending.is_some(),
            "an unfinished transaction must stay visible to the next run"
        );
    }

    /// (7) The lock is taken before anything is read or written.
    #[test]
    fn concurrent_setups_are_serialized_by_the_lock() {
        let mut store = settled("gen-a");
        let mut caddy = FakeCaddy::default();
        activate(&mut store, &mut caddy, "gen-a", &fragments()).expect("no-op");
        assert_eq!(store.locks, 1);
        assert_eq!(store.steps.first().map(String::as_str), Some("lock"));
    }

    /// (8) An incomplete generation directory is never treated as active, even
    /// when `current` points straight at it.
    #[test]
    fn an_incomplete_generation_is_never_referenced_as_active() {
        let mut store = FakeStore {
            current: Some("gen-half".into()),
            activated: Some("gen-a".into()),
            pending: Some(journal("gen-half", Some("gen-a"), false)),
            receipt: Some("gen-a".into()),
            // gen-half is deliberately absent from `generations`.
            generations: BTreeMap::from([("gen-a".into(), true)]),
            ..Default::default()
        };
        let mut caddy = FakeCaddy::default();

        recover(&mut store, &mut caddy).expect("recovers");
        assert_eq!(
            store.current.as_deref(),
            Some("gen-a"),
            "the half-written generation must be abandoned, not activated"
        );
        assert_eq!(caddy.validates, 0, "it must not even be offered to caddy");
    }

    /// (9) A first install has no previous generation; rolling back means
    /// removing `current`, not leaving it on something never confirmed.
    #[test]
    fn a_first_install_rolls_back_to_no_generation_at_all() {
        let mut store = FakeStore::default();
        store.generations.insert("gen-a".into(), true);
        let mut caddy = FakeCaddy {
            fail_all_reloads: true,
            ..Default::default()
        };

        let error = activate(&mut store, &mut caddy, "gen-a", &fragments()).expect_err("fails");
        assert!(format!("{error:#}").contains("activation failed"));
        assert_eq!(store.current, None, "current is removed, not left dangling");
        assert_eq!(store.activated, None);
        assert!(store.pending.is_none(), "the rollback completed");
    }

    /// A successful first activation records the receipt only after the reload.
    #[test]
    fn a_first_activation_stops_at_reloaded_pending_probe() {
        let mut store = FakeStore::default();
        let mut caddy = FakeCaddy::default();
        let outcome = activate(&mut store, &mut caddy, "gen-a", &fragments()).expect("activates");
        assert_eq!(
            outcome,
            ActivationOutcome::ReloadedPendingProbe {
                candidate: "gen-a".into(),
                previous: None,
            }
        );
        assert_eq!(store.current.as_deref(), Some("gen-a"));
        assert_eq!(
            store.activated, None,
            "a reload is not a confirmation — the marker is the probe stage's to set"
        );
        assert_eq!(store.receipt, None);
        assert!(
            store.pending.as_ref().is_some_and(|j| j.reload_succeeded),
            "the transaction stays open for the probe"
        );

        let order: Vec<&str> = store.steps.iter().map(String::as_str).collect();
        let swap = order.iter().position(|s| *s == "set_current").unwrap();
        let journal = order.iter().position(|s| *s == "write_pending").unwrap();
        assert!(journal < swap, "the journal precedes the swap: {order:?}");
        assert!(
            !order.contains(&"write_receipt"),
            "no receipt may be written before a probe: {order:?}"
        );
    }
}
