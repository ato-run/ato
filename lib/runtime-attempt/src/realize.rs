//! How a candidate comes to be running is the executor's; what is observed
//! and decided about it is the attempt's.
//!
//! ```text
//! run_attempt:  admission → start record → realizer.realize() → observe
//!               → verify → receipt → stop | hand off
//! realizer:     build, materialize, fetch declared objects, launch, wait
//!               until its ports accept connections
//! ```
//!
//! A Formation realizer builds from source; a portable Run realizer unpacks
//! a validated `.capsule` and starts it. Neither decides K.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use ato_formation::request::{AttemptFailure, RealizationEvidence, RuntimeProfile};
use ato_formation::verify::VerificationExecutionEvidence;

use crate::executor::ExecutedCandidate;

/// A candidate that is running, owned by whoever holds it. Dropping the
/// owning value attempts cleanup as a last defence; only explicit stop
/// confirms cessation. Unconfirmed workloads and scratch remain for recovery.
pub trait RunningCandidate: Any {
    /// Where each logical port answers: port id to `http://127.0.0.1:<port>`.
    fn endpoints(&self) -> &BTreeMap<String, String>;
    /// `Some(description)` once the candidate has exited on its own.
    fn exited(&mut self) -> Result<Option<String>>;
    /// Stop the candidate and remove the runtime scratch its realization
    /// owns (not a build workspace, not an unpublished artifact).
    fn stop(self: Box<Self>) -> Result<()>;
    /// For the owner that needs its own type back (a static Run reads the
    /// page state it kept before stopping).
    fn as_any(&self) -> &dyn Any;
}

/// What a realizer brought up.
pub struct Realized {
    pub candidate: Box<dyn RunningCandidate>,
    /// How it was realized, for the attempt's evidence.
    pub evidence: Option<RealizationEvidence>,
    /// Receipt-safe execution facts: realization kind, runtime executable
    /// and version, pid, fetched objects, portability profile. The attempt
    /// fills in its own identifiers.
    pub execution: VerificationExecutionEvidence,
    /// What a Formation keeps if the candidate is verified; `None` when the
    /// candidate runs something that already exists (a published bundle).
    pub kept: Option<ExecutedCandidate>,
}

/// Why a realizer could not bring a candidate up.
pub enum RealizeFailure {
    /// Nothing ran to be observed: the build, the materialization or a
    /// declared object fetch failed.
    Execution(anyhow::Error),
    /// The candidate was prepared and could not be started or reached.
    /// Whatever was started is gone (see `evidence.destroyed`).
    Launch {
        error: anyhow::Error,
        evidence: Option<Box<RealizationEvidence>>,
    },
    /// Part of the candidate started, the start then failed, and what had
    /// started could not be confirmed stopped. It and its runtime scratch are
    /// kept for recovery: this is not "no candidate was realized".
    Abandoned {
        error: anyhow::Error,
        /// Why the stop is not confirmed.
        cleanup: String,
        /// What was left, by the ids recovery can find it by.
        resources: Vec<String>,
    },
}

impl From<anyhow::Error> for RealizeFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Execution(error)
    }
}

/// Makes a candidate runnable and starts it — the executor side of an
/// attempt.
pub trait CandidateRealizer {
    /// Whether this realizer can run the candidate on this Runtime
    /// (containment, toolchains, build network). `None` when it can. Asked
    /// before the start record, so a refusal runs nothing.
    fn admit(&self, profile: &RuntimeProfile) -> Option<AttemptFailure>;

    /// Bring the candidate up, ready to be observed. Called after the start
    /// record is durable.
    fn realize(&self, attempt_id: &str, attempt_root: &Path) -> Result<Realized, RealizeFailure>;
}

/// A verified candidate still running, handed to the caller. Dropping it
/// attempts cleanup; [`LiveCandidate::stop`] reports whether cessation was confirmed.
pub struct LiveCandidate {
    inner: Box<dyn RunningCandidate>,
}

impl LiveCandidate {
    pub(crate) fn new(inner: Box<dyn RunningCandidate>) -> Self {
        Self { inner }
    }

    /// Where the candidate answers, for the first logical port.
    pub fn endpoint(&self) -> Option<String> {
        self.inner
            .endpoints()
            .values()
            .next()
            .map(|base| format!("{base}/"))
    }

    /// Every logical port and where it answers.
    pub fn endpoints(&self) -> &BTreeMap<String, String> {
        self.inner.endpoints()
    }

    /// `Some(description)` once the candidate has exited on its own.
    pub fn exited(&mut self) -> Result<Option<String>> {
        self.inner.exited()
    }

    /// The realizer's own type, for an owner that knows it.
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.inner.as_any().downcast_ref::<T>()
    }

    /// Stop the candidate and remove the runtime scratch it owns.
    pub fn stop(self) -> Result<()> {
        self.inner.stop()
    }
}

/// Why a candidate's stop did not fully succeed. A [`RunningCandidate::stop`]
/// error of any other type is read as [`StopClass::Unconfirmed`]: a stop that
/// cannot say what happened is never taken as a confirmed one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CandidateStopFailure {
    /// Something may still be running: nothing it writes can be taken as
    /// final, and what it owns is kept.
    #[error("the candidate could not be confirmed stopped: {reason}")]
    Unconfirmed {
        reason: String,
        /// What was left, by the ids an operator or recovery can find it by.
        resources: Vec<String>,
    },
    /// Everything is confirmed stopped; removing its runtime scratch failed.
    #[error("the candidate stopped, and its runtime scratch was not removed: {reason}")]
    ScratchKept { reason: String },
}

/// What a candidate's stop established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopClass {
    /// Confirmed stopped, and its runtime scratch removed.
    Confirmed,
    /// Confirmed stopped; its runtime scratch was not removed.
    ScratchKept { reason: String },
    /// Not confirmed stopped.
    Unconfirmed {
        reason: String,
        resources: Vec<String>,
    },
}

impl StopClass {
    pub fn of(stopped: &Result<()>) -> Self {
        match stopped {
            Ok(()) => Self::Confirmed,
            Err(error) => match error.downcast_ref::<CandidateStopFailure>() {
                Some(CandidateStopFailure::ScratchKept { reason }) => Self::ScratchKept {
                    reason: reason.clone(),
                },
                Some(CandidateStopFailure::Unconfirmed { reason, resources }) => {
                    Self::Unconfirmed {
                        reason: reason.clone(),
                        resources: resources.clone(),
                    }
                }
                None => Self::Unconfirmed {
                    reason: format!("{error:#}"),
                    resources: Vec::new(),
                },
            },
        }
    }

    /// Whether whatever ran is known not to be running any more.
    pub fn is_confirmed(&self) -> bool {
        !matches!(self, Self::Unconfirmed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_error_that_cannot_say_what_happened_is_unconfirmed() {
        assert_eq!(StopClass::of(&Ok(())), StopClass::Confirmed);
        let untyped = StopClass::of(&Err(anyhow::anyhow!("terminate failed")));
        assert!(!untyped.is_confirmed(), "{untyped:?}");
        let kept = StopClass::of(&Err(anyhow::Error::new(
            CandidateStopFailure::ScratchKept {
                reason: "busy".to_owned(),
            },
        )));
        assert!(kept.is_confirmed());
        let unconfirmed = StopClass::of(&Err(anyhow::Error::new(
            CandidateStopFailure::Unconfirmed {
                reason: "docker did not answer".to_owned(),
                resources: vec!["container:c1".to_owned()],
            },
        )
        .context("stop the Run")));
        assert_eq!(
            unconfirmed,
            StopClass::Unconfirmed {
                reason: "docker did not answer".to_owned(),
                resources: vec!["container:c1".to_owned()],
            }
        );
    }
}
