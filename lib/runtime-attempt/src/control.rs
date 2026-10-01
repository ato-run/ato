//! Operational deadline and phase evidence. Neither changes K or D identity.
use std::cell::RefCell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ato_formation::failure::{FailureStage, FormationFailure};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Source,
    Dependencies,
    Build,
    Launch,
    Verification,
    Backoff,
    Cleanup,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseTiming {
    pub phase: AttemptPhase,
    pub started_at_ms: u64,
    pub elapsed_ms: u64,
}

/// Absolute, frozen deadline with a monotonic bound inside one process. A wall
/// clock adjustment can shorten a deadline but cannot extend a running attempt.
pub struct ExecutionControl {
    deadline_ms: u64,
    started: Instant,
    initial_remaining: Duration,
    timings: RefCell<Vec<PhaseTiming>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

impl ExecutionControl {
    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            deadline_ms,
            started: Instant::now(),
            initial_remaining: Duration::from_millis(deadline_ms.saturating_sub(now_ms())),
            timings: RefCell::new(Vec::new()),
        }
    }

    pub fn remaining(&self, phase: AttemptPhase) -> Result<Duration> {
        let remaining = Duration::from_millis(self.deadline_ms.saturating_sub(now_ms())).min(
            self.initial_remaining
                .saturating_sub(self.started.elapsed()),
        );
        if remaining.is_zero() {
            let stage = match phase {
                AttemptPhase::Dependencies | AttemptPhase::Build => FailureStage::Build,
                AttemptPhase::Verification => FailureStage::Verification,
                _ => FailureStage::Admission,
            };
            return Err(FormationFailure::new(
                "round_deadline_exceeded",
                stage,
                format!("the frozen execution deadline elapsed during {phase:?}"),
            )
            .into());
        }
        Ok(remaining)
    }

    pub fn cap(&self, phase: AttemptPhase, configured: Duration) -> Result<Duration> {
        Ok(configured.min(self.remaining(phase)?))
    }

    pub fn phase(&self, phase: AttemptPhase) -> Result<PhaseTimer<'_>> {
        self.remaining(phase)?;
        Ok(self.timer(phase))
    }

    /// Cleanup remains possible after expiry; it never starts another workload.
    pub fn cleanup(&self) -> PhaseTimer<'_> {
        self.timer(AttemptPhase::Cleanup)
    }

    fn timer(&self, phase: AttemptPhase) -> PhaseTimer<'_> {
        PhaseTimer {
            control: self,
            phase,
            started_at_ms: now_ms(),
            started: Instant::now(),
        }
    }

    pub fn timings(&self) -> Vec<PhaseTiming> {
        self.timings.borrow().clone()
    }
}

pub struct PhaseTimer<'a> {
    control: &'a ExecutionControl,
    phase: AttemptPhase,
    started_at_ms: u64,
    started: Instant,
}
impl Drop for PhaseTimer<'_> {
    fn drop(&mut self) {
        self.control.timings.borrow_mut().push(PhaseTiming {
            phase: self.phase,
            started_at_ms: self.started_at_ms,
            elapsed_ms: self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_control_refuses_effects_but_records_cleanup() {
        let control = ExecutionControl::new(now_ms().saturating_sub(1));
        for phase in [
            AttemptPhase::Source,
            AttemptPhase::Build,
            AttemptPhase::Launch,
            AttemptPhase::Verification,
            AttemptPhase::Backoff,
        ] {
            assert!(control.phase(phase).is_err());
        }
        drop(control.cleanup());
        assert_eq!(control.timings().len(), 1);
        assert_eq!(control.timings()[0].phase, AttemptPhase::Cleanup);
    }

    #[test]
    fn an_active_phase_does_not_receive_a_fresh_deadline() {
        let control = ExecutionControl::new(now_ms() + 40);
        let build = control.phase(AttemptPhase::Build).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert!(control.remaining(AttemptPhase::Build).is_err());
        drop(build);
        assert!(control.phase(AttemptPhase::Launch).is_err());
        assert!(
            ExecutionControl::new(control.deadline_ms)
                .phase(AttemptPhase::Launch)
                .is_err()
        );
        assert!(control.timings()[0].elapsed_ms >= 40);
    }
}
