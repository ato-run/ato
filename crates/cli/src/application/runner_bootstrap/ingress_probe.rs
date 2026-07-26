//! Confirming that a generation is actually being SERVED, and only then
//! recording it.
//!
//! This is the stage [`super::ingress_activation`] deliberately stops short of.
//! A reload proves acceptance; nothing about it proves an origin now answers
//! from the new routes.
//!
//! # Why a 200 is not a confirmation
//!
//! Every generation answers the same well-known path with the same `ok`. So
//! does the OLD generation, and so does an unrelated vhost that happens to
//! catch the request. A probe that accepted a 200 would pass in exactly the
//! cases it exists to catch: a reload that silently did nothing, a rollback
//! that already happened, a request that never reached the origin it named.
//!
//! The generation therefore identifies ITSELF, from the Caddy route, and the
//! probe checks that identity:
//!
//! ```text
//! probe 1  the exact origin, under its exact Host, returns the CANDIDATE's
//!          generation id AND its full digest — proving Caddy is serving these
//!          routes for this hostname
//! probe 2  the same origin reaches its upstream and satisfies the readiness
//!          contract, with the candidate's marker still on the response
//! ```
//!
//! Only after both, for every origin, is anything recorded:
//!
//! ```text
//! activated-generation = candidate
//! receipt              = written
//! activation.pending   = removed
//! ```
//!
//! # Failure is not a shrug
//!
//! A failed probe rolls back — and then probes the PREVIOUS generation too,
//! because a rollback that was not confirmed is exactly as unproven as the
//! activation that just failed. If the rollback reload or the rollback probe
//! fails, both errors are reported and the journal is left for the next run.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::ingress_activation::{
    ActivationFailure, ActivationOutcome, CaddyControl, GenerationStore,
};
use super::official_preview::GenerationIdentity;

/// Which generation a probe expects to find answering.
///
/// The candidate is known in BOTH halves — it was just derived — so both are
/// checked. A PREVIOUS generation is known only by its id: its full digest is
/// not recoverable from the id, and it is not written down anywhere the
/// rollback path can read. Rather than fabricate one (an empty expected digest
/// would make every rollback confirmation fail), the asymmetry is explicit, and
/// the id still answers the question a rollback asks — did the restore take
/// effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpectedGeneration {
    pub id: String,
    pub digest: Option<String>,
}

impl ExpectedGeneration {
    /// Both halves, for a generation this process just built.
    pub(crate) fn exact(identity: &GenerationIdentity) -> Self {
        Self {
            id: identity.id.clone(),
            digest: Some(identity.digest.clone()),
        }
    }

    /// The id alone, for a generation only known by name.
    pub(crate) fn by_id(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            digest: None,
        }
    }
}

/// One origin to confirm, and what "ready" means for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeTarget {
    /// The exact hostname the request must be made under.
    pub origin: String,
    /// The readiness path behind the origin — reaches the upstream, unlike the
    /// marker path, which Caddy answers itself.
    pub readiness_path: String,
}

/// What a probe saw. The marker is separate from the status because "answered,
/// but as the wrong generation" is the interesting failure and must not be
/// collapsed into "answered".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeResponse {
    pub status: u16,
    /// The generation identity the response carried, if any.
    pub marker: Option<GenerationIdentity>,
}

/// The HTTP seam. Kept narrow: a request to one origin, under an exact Host, on
/// one path.
pub(crate) trait IngressProbe {
    fn get(&mut self, origin: &str, path: &str) -> Result<ProbeResponse>;
}

/// How long the probe keeps retrying one target, and how often.
///
/// Bounded on purpose: a reload lands asynchronously, so a first attempt can
/// legitimately still see the old generation — but "eventually" is not a
/// confirmation, and an unbounded wait would hold the activation lock while the
/// operator has no idea what is happening.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbeBudget {
    pub deadline: Duration,
    pub interval: Duration,
}

impl Default for ProbeBudget {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(20),
            interval: Duration::from_millis(250),
        }
    }
}

/// Why a confirmation failed. Each variant names something an operator can act
/// on — in particular, "the previous generation is still answering" is a
/// different problem from "nothing answered".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeFailure {
    Unreachable {
        origin: String,
        detail: String,
    },
    Status {
        origin: String,
        path: String,
        status: u16,
    },
    MissingMarker {
        origin: String,
        path: String,
    },
    WrongGeneration {
        origin: String,
        expected: ExpectedGeneration,
        found: GenerationIdentity,
    },
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeFailure::Unreachable { origin, detail } => {
                write!(f, "{origin} did not answer: {detail}")
            }
            ProbeFailure::Status {
                origin,
                path,
                status,
            } => write!(f, "{origin}{path} answered {status}"),
            ProbeFailure::MissingMarker { origin, path } => write!(
                f,
                "{origin}{path} answered without a generation marker — the route serving it \
                 is not one this tool generated"
            ),
            ProbeFailure::WrongGeneration {
                origin,
                expected,
                found,
            } => write!(
                f,
                "{origin} is served by generation {} ({}), not the expected {} — \
                 the reload was accepted but these routes are not the ones answering",
                found.id, found.digest, expected.id
            ),
        }
    }
}

impl std::error::Error for ProbeFailure {}

/// Confirm one target: Caddy is serving `expected` for it, and its upstream is
/// ready under the same generation.
fn confirm_target(
    probe: &mut dyn IngressProbe,
    target: &ProbeTarget,
    expected: &ExpectedGeneration,
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<(), ProbeFailure> {
    // Probe 1 — Caddy's own answer. Retried, because a reload lands
    // asynchronously and the old generation answering once is not yet a
    // failure; it only becomes one when it is still answering at the deadline.
    attempt_until(budget, || {
        let response =
            probe
                .get(&target.origin, marker_path)
                .map_err(|error| ProbeFailure::Unreachable {
                    origin: target.origin.clone(),
                    detail: error.to_string(),
                })?;
        check_marker(&target.origin, marker_path, &response, expected)
    })?;

    // Probe 2 — the upstream, through the same origin, still under the
    // candidate. Checking the marker again is what makes this end-to-end: a
    // readiness answer without it means the request reached something other
    // than the routes just activated.
    attempt_until(budget, || {
        let response = probe
            .get(&target.origin, &target.readiness_path)
            .map_err(|error| ProbeFailure::Unreachable {
                origin: target.origin.clone(),
                detail: error.to_string(),
            })?;
        if !(200..400).contains(&response.status) {
            return Err(ProbeFailure::Status {
                origin: target.origin.clone(),
                path: target.readiness_path.clone(),
                status: response.status,
            });
        }
        check_marker(&target.origin, &target.readiness_path, &response, expected)
    })
}

fn check_marker(
    origin: &str,
    path: &str,
    response: &ProbeResponse,
    expected: &ExpectedGeneration,
) -> Result<(), ProbeFailure> {
    let Some(found) = response.marker.as_ref() else {
        return Err(ProbeFailure::MissingMarker {
            origin: origin.to_string(),
            path: path.to_string(),
        });
    };
    // Both halves when both are known: matching only the short id would accept
    // a generation that merely collided with the handle, and matching only the
    // digest would accept a body whose id names something else.
    let digest_disagrees = expected
        .digest
        .as_ref()
        .is_some_and(|digest| *digest != found.digest);
    if found.id != expected.id || digest_disagrees {
        return Err(ProbeFailure::WrongGeneration {
            origin: origin.to_string(),
            expected: expected.clone(),
            found: found.clone(),
        });
    }
    Ok(())
}

/// Retry until the deadline, returning the LAST failure rather than the first —
/// the last one is what was still true when the budget ran out.
fn attempt_until(
    budget: ProbeBudget,
    mut attempt: impl FnMut() -> Result<(), ProbeFailure>,
) -> Result<(), ProbeFailure> {
    let deadline = Instant::now() + budget.deadline;
    let mut last = attempt();
    while last.is_err() && Instant::now() < deadline {
        std::thread::sleep(budget.interval);
        last = attempt();
    }
    last
}

/// The outcome of confirming an activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Confirmation {
    /// Every origin is served by the candidate and ready. Recorded.
    Confirmed { generation: String },
    /// The candidate did not confirm; the previous generation was restored AND
    /// re-confirmed. The original failure is carried.
    RolledBack { failure: String },
}

/// Take a [`ActivationOutcome::ReloadedPendingProbe`] to a settled state.
///
/// This is the ONLY caller allowed to move the activated marker, and it does so
/// after — never before — every target has answered as the candidate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn confirm_activation(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    candidate: &GenerationIdentity,
    previous: Option<&str>,
    targets: &[ProbeTarget],
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<Confirmation> {
    let expected = ExpectedGeneration::exact(candidate);
    let mut failure: Option<ProbeFailure> = None;
    for target in targets {
        if let Err(error) = confirm_target(probe, target, &expected, marker_path, budget) {
            failure = Some(error);
            break;
        }
    }

    let Some(failure) = failure else {
        // Order matters and is the same one recovery reads back: the marker
        // first (it is what makes the generation confirmed), then the receipt,
        // then the journal. A crash between any two of them is a state recovery
        // already knows how to finish.
        store.write_activated(Some(&candidate.id))?;
        store.write_receipt(Some(&candidate.id))?;
        store.clear_pending()?;
        return Ok(Confirmation::Confirmed {
            generation: candidate.id.clone(),
        });
    };

    let primary = failure.to_string();
    match roll_back_and_confirm(store, caddy, probe, previous, targets, marker_path, budget) {
        Ok(()) => Ok(Confirmation::RolledBack { failure: primary }),
        Err(rollback) => Err(ActivationFailure {
            primary,
            rollback: Some(rollback.to_string()),
        }
        .into()),
    }
}

/// Restore the previous generation and prove IT is serving.
///
/// A rollback that is merely requested is exactly as unproven as the activation
/// that just failed — and it is the state the box is being left in, so it is
/// the one that most needs checking.
fn roll_back_and_confirm(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    previous: Option<&str>,
    targets: &[ProbeTarget],
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<()> {
    store.set_current(previous)?;
    let Some(previous) = previous else {
        // A first install: there is no generation to reload into and nothing to
        // probe. Clearing the confirmed state is the honest record — the box is
        // serving whatever it served before this tool ever ran.
        store.write_activated(None)?;
        store.clear_pending()?;
        return Ok(());
    };

    caddy.reload()?;
    let expected = ExpectedGeneration::by_id(previous);
    for target in targets {
        confirm_target(probe, target, &expected, marker_path, budget)
            .map_err(|error| anyhow::anyhow!("after rolling back: {error}"))?;
    }
    store.write_activated(Some(previous))?;
    store.write_receipt(Some(previous))?;
    store.clear_pending()?;
    Ok(())
}

/// Confirm whatever an interrupted activation left behind.
///
/// `ReloadedPendingProbe` is the only entry point: it is the state in which the
/// disk has moved but nothing has been confirmed, and it is reached both by a
/// fresh activation and by recovery after a crash.
#[allow(clippy::too_many_arguments)]
pub(crate) fn confirm_outcome(
    store: &mut dyn GenerationStore,
    caddy: &mut dyn CaddyControl,
    probe: &mut dyn IngressProbe,
    outcome: &ActivationOutcome,
    identity_of: impl Fn(&str) -> Result<GenerationIdentity>,
    targets: &[ProbeTarget],
    marker_path: &str,
    budget: ProbeBudget,
) -> Result<Option<Confirmation>> {
    let ActivationOutcome::ReloadedPendingProbe {
        candidate,
        previous,
    } = strip_recovery(outcome)
    else {
        return Ok(None);
    };
    let candidate = identity_of(candidate)?;
    confirm_activation(
        store,
        caddy,
        probe,
        &candidate,
        previous.as_deref(),
        targets,
        marker_path,
        budget,
    )
    .map(Some)
}

/// A recovery wrapper carries the outcome that matters inside it.
fn strip_recovery(outcome: &ActivationOutcome) -> &ActivationOutcome {
    match outcome {
        ActivationOutcome::Recovered(inner) => strip_recovery(inner),
        other => other,
    }
}

/// An [`IngressProbe`] over plain HTTP to a loopback listener, with the origin
/// carried in the `Host` header.
///
/// Loopback and `Host` rather than DNS and TLS: the question is whether THIS
/// box's Caddy is serving the routes, and resolving the public name would send
/// the probe out to whatever the internet currently points at — which is a
/// different question, and one that fails for reasons that have nothing to do
/// with the activation.
pub(crate) struct LoopbackHttpProbe {
    base: String,
    timeout: Duration,
}

impl LoopbackHttpProbe {
    pub(crate) fn new(base: impl Into<String>, timeout: Duration) -> Result<Self> {
        let base = base.into();
        if !base.starts_with("http://127.0.0.1") && !base.starts_with("http://[::1]") {
            bail!(
                "the ingress probe must target a loopback listener (got {base:?}) — \
                 probing the public name asks a different question"
            );
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            timeout,
        })
    }
}

impl IngressProbe for LoopbackHttpProbe {
    fn get(&mut self, origin: &str, path: &str) -> Result<ProbeResponse> {
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            // The origin is carried in the Host header, so a redirect would
            // send the probe somewhere it never named.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let response = client
            .get(format!("{}{path}", self.base))
            .header(reqwest::header::HOST, origin)
            .send()?;
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        Ok(ProbeResponse {
            status,
            marker: parse_marker(&body),
        })
    }
}

/// Read a marker body, or `None` when the response is not one.
///
/// Deliberately tolerant about the response being something else entirely (an
/// app's own 200, an error page) and strict about a body that CLAIMS to be a
/// marker: a half-parsed marker would be worse than none.
fn parse_marker(body: &str) -> Option<GenerationIdentity> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let object = value.as_object()?;
    let id = object.get("generation_id")?.as_str()?;
    let digest = object.get("generation_digest")?.as_str()?;
    if id.is_empty() || digest.is_empty() {
        return None;
    }
    Some(GenerationIdentity {
        id: id.to_string(),
        digest: digest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::runner_bootstrap::ingress_activation::PendingJournal;
    use crate::application::runner_bootstrap::official_preview::GENERATION_MARKER_PATH;

    fn identity(id: &str) -> GenerationIdentity {
        GenerationIdentity {
            id: id.to_string(),
            digest: format!("{id}{}", "0".repeat(64 - id.len())),
        }
    }

    fn targets() -> Vec<ProbeTarget> {
        vec![ProbeTarget {
            origin: "s0.runner-abc.runner.ato.run".into(),
            readiness_path: "/.well-known/ato-runner-ingress".into(),
        }]
    }

    fn fast() -> ProbeBudget {
        ProbeBudget {
            deadline: Duration::from_millis(30),
            interval: Duration::from_millis(5),
        }
    }

    /// What the box is currently serving, shared between the fake Caddy and the
    /// fake probe.
    ///
    /// Modelling the BOX rather than scripting answers is what makes the
    /// probe-1/probe-2 distinction real: a reload changes what is served, and
    /// the probe then observes it — exactly the relationship the production
    /// code depends on.
    #[derive(Clone, Default)]
    struct Served {
        generation: std::rc::Rc<std::cell::RefCell<Option<GenerationIdentity>>>,
        readiness_status: std::rc::Rc<std::cell::RefCell<u16>>,
    }

    impl Served {
        fn new(generation: Option<&GenerationIdentity>, readiness_status: u16) -> Self {
            Self {
                generation: std::rc::Rc::new(std::cell::RefCell::new(generation.cloned())),
                readiness_status: std::rc::Rc::new(std::cell::RefCell::new(readiness_status)),
            }
        }

        fn set(&self, generation: Option<&GenerationIdentity>) {
            *self.generation.borrow_mut() = generation.cloned();
        }
    }

    /// Answers from [`Served`]. Nothing served at all ⇒ unreachable, which is
    /// what a box with no route does.
    struct FakeProbe {
        served: Served,
        /// Responses that carry a 200 but no marker — something else answering.
        markerless: bool,
        calls: Vec<(String, String)>,
    }

    impl FakeProbe {
        fn new(served: Served) -> Self {
            Self {
                served,
                markerless: false,
                calls: Vec::new(),
            }
        }
    }

    impl IngressProbe for FakeProbe {
        fn get(&mut self, origin: &str, path: &str) -> Result<ProbeResponse> {
            self.calls.push((origin.to_string(), path.to_string()));
            let generation = self.served.generation.borrow().clone();
            let Some(generation) = generation else {
                bail!("connection refused");
            };
            if self.markerless {
                return Ok(ProbeResponse {
                    status: 200,
                    marker: None,
                });
            }
            let status = if path == GENERATION_MARKER_PATH {
                200
            } else {
                *self.served.readiness_status.borrow()
            };
            Ok(ProbeResponse {
                status,
                marker: Some(generation),
            })
        }
    }

    #[derive(Default)]
    struct FakeStore {
        current: Option<String>,
        activated: Option<String>,
        receipt: Option<String>,
        pending: Option<PendingJournal>,
        complete: Vec<String>,
        steps: Vec<String>,
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
            _digest: &str,
            _fragments: &[super::super::official_preview::GeneratedFragment],
        ) -> Result<()> {
            Ok(())
        }
        fn generation_complete(&self, digest: &str) -> Result<bool> {
            Ok(self.complete.iter().any(|d| d == digest))
        }
        fn generation_matches(
            &self,
            digest: &str,
            _fragments: &[super::super::official_preview::GeneratedFragment],
        ) -> Result<bool> {
            self.generation_complete(digest)
        }
        fn write_pending(&mut self, journal: &PendingJournal) -> Result<()> {
            self.pending = Some(journal.clone());
            Ok(())
        }
        fn clear_pending(&mut self) -> Result<()> {
            self.steps.push("clear_pending".into());
            self.pending = None;
            Ok(())
        }
        fn read_receipt(&self) -> Result<Option<String>> {
            Ok(self.receipt.clone())
        }
        fn write_receipt(&mut self, digest: Option<&str>) -> Result<()> {
            self.steps.push("write_receipt".into());
            self.receipt = digest.map(str::to_string);
            Ok(())
        }
        fn set_current(&mut self, digest: Option<&str>) -> Result<()> {
            self.steps.push("set_current".into());
            self.current = digest.map(str::to_string);
            Ok(())
        }
        fn write_activated(&mut self, digest: Option<&str>) -> Result<()> {
            self.steps.push("write_activated".into());
            self.activated = digest.map(str::to_string);
            Ok(())
        }
    }

    /// A reload makes the box serve `serves_on_reload` — the same thing a real
    /// reload does, so a rollback in these tests genuinely changes what the
    /// probe then sees.
    struct FakeCaddy {
        served: Served,
        serves_on_reload: Option<GenerationIdentity>,
        reloads: u32,
        fail_reload: bool,
    }

    impl FakeCaddy {
        fn reloading_into(served: Served, generation: Option<&GenerationIdentity>) -> Self {
            Self {
                served,
                serves_on_reload: generation.cloned(),
                reloads: 0,
                fail_reload: false,
            }
        }
    }

    impl CaddyControl for FakeCaddy {
        fn validate(&mut self, _digest: &str) -> Result<()> {
            Ok(())
        }
        fn reload(&mut self) -> Result<()> {
            self.reloads += 1;
            if self.fail_reload {
                bail!("injected reload failure");
            }
            self.served.set(self.serves_on_reload.as_ref());
            Ok(())
        }
    }

    fn mid_activation(candidate: &str, previous: Option<&str>) -> FakeStore {
        FakeStore {
            current: Some(candidate.into()),
            activated: previous.map(str::to_string),
            receipt: previous.map(str::to_string),
            pending: Some(PendingJournal {
                candidate: candidate.into(),
                previous: previous.map(str::to_string),
                reload_succeeded: true,
            }),
            complete: vec![candidate.into()],
            ..Default::default()
        }
    }

    fn confirm(
        store: &mut FakeStore,
        caddy: &mut FakeCaddy,
        probe: &mut FakeProbe,
        candidate: &GenerationIdentity,
        previous: Option<&GenerationIdentity>,
    ) -> Result<Confirmation> {
        confirm_activation(
            store,
            caddy,
            probe,
            candidate,
            previous.map(|identity| identity.id.as_str()),
            &targets(),
            GENERATION_MARKER_PATH,
            fast(),
        )
    }

    /// Both probes pass ⇒ the marker, then the receipt, then the journal — in
    /// that order, because recovery reads them back in it.
    #[test]
    fn a_confirmed_candidate_is_recorded_marker_then_receipt_then_journal() {
        let candidate = identity("gen-b");
        let served = Served::new(Some(&candidate), 200);
        let mut store = mid_activation("gen-b", Some("gen-a"));
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);

        let outcome = confirm(
            &mut store,
            &mut caddy,
            &mut probe,
            &candidate,
            Some(&identity("gen-a")),
        )
        .expect("confirms");
        assert_eq!(
            outcome,
            Confirmation::Confirmed {
                generation: "gen-b".into()
            }
        );
        assert_eq!(store.activated.as_deref(), Some("gen-b"));
        assert_eq!(store.receipt.as_deref(), Some("gen-b"));
        assert!(store.pending.is_none());
        assert_eq!(
            store.steps,
            vec!["write_activated", "write_receipt", "clear_pending"]
        );
        assert_eq!(caddy.reloads, 0, "a confirmation reloads nothing");
    }

    /// Probe 1 fails — Caddy accepted the reload but the PREVIOUS generation is
    /// still answering. Nothing is confirmed, and the rollback is proven.
    #[test]
    fn a_probe_one_failure_rolls_back_and_reconfirms_the_previous_generation() {
        let candidate = identity("gen-b");
        let previous = identity("gen-a");
        let served = Served::new(Some(&previous), 200);
        let mut store = mid_activation("gen-b", Some("gen-a"));
        let mut caddy = FakeCaddy::reloading_into(served.clone(), Some(&previous));
        let mut probe = FakeProbe::new(served);

        let outcome = confirm(
            &mut store,
            &mut caddy,
            &mut probe,
            &candidate,
            Some(&previous),
        )
        .expect("rolls back");
        match outcome {
            Confirmation::RolledBack { failure } => {
                assert!(
                    failure.contains("is served by generation gen-a"),
                    "{failure}"
                );
                assert!(failure.contains("not the expected gen-b"), "{failure}");
            }
            other => panic!("expected RolledBack, got {other:?}"),
        }
        assert_eq!(store.current.as_deref(), Some("gen-a"));
        assert_eq!(store.activated.as_deref(), Some("gen-a"));
        assert_eq!(store.receipt.as_deref(), Some("gen-a"));
        assert!(store.pending.is_none());
        assert_eq!(caddy.reloads, 1, "the previous generation was reloaded");
    }

    /// Probe 1 passes but the upstream is not ready ⇒ still a rollback. Caddy
    /// serving the right routes is not the same as the app being reachable —
    /// and this is the case that distinguishes the two stages.
    #[test]
    fn a_probe_two_failure_rolls_back_even_though_caddy_serves_the_candidate() {
        let candidate = identity("gen-b");
        let previous = identity("gen-a");
        // Caddy IS serving the candidate; the upstream behind it answers 502.
        let served = Served::new(Some(&candidate), 502);
        let mut store = mid_activation("gen-b", Some("gen-a"));
        let mut caddy = FakeCaddy::reloading_into(served.clone(), Some(&previous));
        let mut probe = FakeProbe::new(served);

        let outcome = confirm(
            &mut store,
            &mut caddy,
            &mut probe,
            &candidate,
            Some(&previous),
        );
        // The rollback probe re-checks readiness, which is still 502 here, so
        // this is the composite path — the assertion that matters is that the
        // CANDIDATE was never confirmed despite probe 1 passing.
        assert!(outcome.is_err(), "an unready upstream must not confirm");
        assert_ne!(store.activated.as_deref(), Some("gen-b"));
        assert_ne!(store.receipt.as_deref(), Some("gen-b"));
        assert!(
            probe
                .calls
                .iter()
                .any(|(_, path)| path == GENERATION_MARKER_PATH),
            "probe 1 ran"
        );
        assert!(
            probe
                .calls
                .iter()
                .any(|(_, path)| path != GENERATION_MARKER_PATH),
            "probe 2 ran, so the failure came from readiness and not from the marker"
        );
    }

    /// A 200 with no marker is not a confirmation: whatever answered is not a
    /// route this tool generated.
    #[test]
    fn a_two_hundred_without_a_marker_is_not_a_confirmation() {
        let candidate = identity("gen-b");
        let served = Served::new(Some(&candidate), 200);
        let mut store = mid_activation("gen-b", None);
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);
        probe.markerless = true;

        confirm(&mut store, &mut caddy, &mut probe, &candidate, None).expect("rolls back");
        assert_eq!(store.activated, None);
        assert_eq!(store.receipt, None);
    }

    /// A marker whose short id matches but whose digest does not is refused —
    /// otherwise a truncation collision would confirm the wrong generation.
    #[test]
    fn a_matching_id_with_a_different_digest_is_refused() {
        let candidate = identity("gen-b");
        let impostor = GenerationIdentity {
            id: candidate.id.clone(),
            digest: "f".repeat(64),
        };
        let served = Served::new(Some(&impostor), 200);
        let mut store = mid_activation("gen-b", None);
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);

        confirm(&mut store, &mut caddy, &mut probe, &candidate, None).expect("rolls back");
        assert_eq!(store.activated, None, "the impostor must not be confirmed");
    }

    /// Rollback reload succeeds but the previous generation still does not
    /// answer ⇒ both errors, journal retained.
    #[test]
    fn a_failed_rollback_probe_reports_both_errors_and_keeps_the_journal() {
        let candidate = identity("gen-b");
        let previous = identity("gen-a");
        // Nothing is served at all, before or after the rollback.
        let served = Served::new(None, 200);
        let mut store = mid_activation("gen-b", Some("gen-a"));
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);

        let error = confirm(
            &mut store,
            &mut caddy,
            &mut probe,
            &candidate,
            Some(&previous),
        )
        .expect_err("composite");
        let message = format!("{error:#}");
        assert!(message.contains("did not answer"), "{message}");
        assert!(message.contains("rollback also failed"), "{message}");
        assert!(
            store.pending.is_some(),
            "an unfinished transaction stays visible to the next run"
        );
    }

    /// A first install whose probe fails ends with no current generation and
    /// nothing confirmed.
    #[test]
    fn a_first_install_that_fails_its_probe_returns_to_no_generation() {
        let candidate = identity("gen-a");
        let served = Served::new(None, 200);
        let mut store = mid_activation("gen-a", None);
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);

        let outcome =
            confirm(&mut store, &mut caddy, &mut probe, &candidate, None).expect("rolls back");
        assert!(matches!(outcome, Confirmation::RolledBack { .. }));
        assert_eq!(store.current, None);
        assert_eq!(store.activated, None);
        assert!(store.pending.is_none());
        assert_eq!(caddy.reloads, 0, "there is nothing to reload into");
    }

    /// Only `ReloadedPendingProbe` enters the probe stage — including when it
    /// arrives wrapped in a recovery.
    #[test]
    fn only_a_reloaded_pending_probe_outcome_is_confirmed() {
        let candidate = identity("gen-b");
        let served = Served::new(Some(&candidate), 200);
        let mut store = mid_activation("gen-b", None);
        let mut caddy = FakeCaddy::reloading_into(served.clone(), None);
        let mut probe = FakeProbe::new(served);
        let resolve = |id: &str| Ok(identity(id));

        assert_eq!(
            confirm_outcome(
                &mut store,
                &mut caddy,
                &mut probe,
                &ActivationOutcome::NoOp,
                resolve,
                &targets(),
                GENERATION_MARKER_PATH,
                fast(),
            )
            .expect("no-op"),
            None
        );

        let wrapped =
            ActivationOutcome::Recovered(Box::new(ActivationOutcome::ReloadedPendingProbe {
                candidate: "gen-b".into(),
                previous: None,
            }));
        let confirmed = confirm_outcome(
            &mut store,
            &mut caddy,
            &mut probe,
            &wrapped,
            resolve,
            &targets(),
            GENERATION_MARKER_PATH,
            fast(),
        )
        .expect("confirms")
        .expect("some");
        assert_eq!(
            confirmed,
            Confirmation::Confirmed {
                generation: "gen-b".into()
            }
        );
    }

    /// The probe retries until its deadline: a reload lands asynchronously, so
    /// the old generation answering once is not yet a failure.
    #[test]
    fn the_probe_retries_within_its_budget() {
        struct Flaky {
            attempts: u32,
            identity: GenerationIdentity,
        }
        impl IngressProbe for Flaky {
            fn get(&mut self, _origin: &str, _path: &str) -> Result<ProbeResponse> {
                self.attempts += 1;
                if self.attempts < 3 {
                    bail!("connection refused");
                }
                Ok(ProbeResponse {
                    status: 200,
                    marker: Some(self.identity.clone()),
                })
            }
        }

        let candidate = identity("gen-b");
        let served = Served::new(Some(&candidate), 200);
        let mut store = mid_activation("gen-b", None);
        let mut caddy = FakeCaddy::reloading_into(served, None);
        let mut probe = Flaky {
            attempts: 0,
            identity: candidate.clone(),
        };
        confirm_activation(
            &mut store,
            &mut caddy,
            &mut probe,
            &candidate,
            None,
            &targets(),
            GENERATION_MARKER_PATH,
            ProbeBudget {
                deadline: Duration::from_secs(2),
                interval: Duration::from_millis(5),
            },
        )
        .expect("confirms after retrying");
        assert!(probe.attempts >= 3);
        assert_eq!(store.activated.as_deref(), Some("gen-b"));
    }

    #[test]
    fn a_marker_body_round_trips_and_a_non_marker_body_is_none() {
        let identity = GenerationIdentity {
            id: "abc123".into(),
            digest: "d".repeat(64),
        };
        assert_eq!(parse_marker(&identity.marker_body()), Some(identity));
        assert_eq!(parse_marker("not json"), None);
        assert_eq!(parse_marker("{}"), None);
        assert_eq!(
            parse_marker(r#"{"generation_id":"","generation_digest":"d"}"#),
            None
        );
    }

    /// The probe must target loopback: resolving the public name asks whether
    /// the internet points here, which is a different question.
    #[test]
    fn the_http_probe_refuses_a_non_loopback_base() {
        assert!(LoopbackHttpProbe::new("http://127.0.0.1:80", Duration::from_secs(1)).is_ok());
        assert!(LoopbackHttpProbe::new("https://runner.ato.run", Duration::from_secs(1)).is_err());
        assert!(LoopbackHttpProbe::new("http://10.0.0.1", Duration::from_secs(1)).is_err());
    }
}
