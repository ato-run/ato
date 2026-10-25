//! Bounded realization readiness. This polls no Contract and never decides K.
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPhase {
    ProcessStarted,
    HttpResponding,
    Ready,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartupProgress {
    pub schema: &'static str,
    pub phase: StartupPhase,
    pub http_responding: bool,
    pub last_http_status: Option<u16>,
    pub elapsed_ms: u128,
    pub failure: Option<&'static str>,
    pub process_exit_code: Option<i32>,
}

pub struct ReadinessObservation {
    pub ready: bool,
    pub http_status: Option<u16>,
    pub detail: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("startup_cancelled")]
pub struct StartupCancelled;

#[derive(Debug, thiserror::Error)]
#[error("startup_timeout")]
pub struct StartupTimeout;

#[derive(Debug, thiserror::Error)]
#[error("startup_process_exited: {exit_code:?}")]
pub struct StartupProcessExited {
    pub exit_code: Option<i32>,
}

pub const PROCESS_STARTUP_RUNTIME_FEATURE: &str = "runtime_feature=process_startup_v1";

pub fn write_progress(path: &Path, progress: &StartupProgress) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("partial");
    std::fs::write(&temporary, serde_json::to_vec(progress)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

/// One immutable deadline, checked around each bounded probe. The callback
/// checks stop, authorization and process exit; successful polling cannot
/// override any of those. Failure evidence is written before returning.
pub fn wait_for_readiness(
    timeout: Duration,
    mut probe: impl FnMut(Duration) -> Result<ReadinessObservation>,
    mut check: impl FnMut() -> Result<()>,
    mut record: impl FnMut(&StartupProgress) -> Result<()>,
) -> Result<StartupProgress> {
    let start = Instant::now();
    let deadline = start
        .checked_add(timeout)
        .ok_or_else(|| anyhow::anyhow!("startup_timeout_invalid"))?;
    let mut progress = StartupProgress {
        schema: "ato.process-startup/1",
        phase: StartupPhase::ProcessStarted,
        http_responding: false,
        last_http_status: None,
        elapsed_ms: 0,
        failure: None,
        process_exit_code: None,
    };
    record(&progress)?;
    let mut last_detail = None;
    let result: Result<()> = (|| {
        loop {
            check()?;
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or(StartupTimeout)?;
            let observed = probe(remaining.min(Duration::from_millis(500)))?;
            last_detail = observed.detail.clone();
            check()?;
            if Instant::now() >= deadline {
                return Err(StartupTimeout.into());
            }
            progress.elapsed_ms = start.elapsed().as_millis();
            if let Some(status) = observed.http_status {
                let changed =
                    progress.last_http_status != Some(status) || !progress.http_responding;
                progress.http_responding = true;
                progress.last_http_status = Some(status);
                progress.phase = StartupPhase::HttpResponding;
                if changed {
                    record(&progress)?;
                }
            }
            if observed.ready {
                progress.phase = StartupPhase::Ready;
                record(&progress)?;
                return Ok(());
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(50)),
            );
        }
    })();
    if let Err(error) = result {
        progress.elapsed_ms = start.elapsed().as_millis();
        progress.phase = if error.is::<StartupCancelled>() {
            StartupPhase::Cancelled
        } else {
            StartupPhase::Failed
        };
        progress.process_exit_code = error
            .downcast_ref::<StartupProcessExited>()
            .and_then(|e| e.exit_code);
        progress.failure = Some(if error.is::<StartupCancelled>() {
            "startup_cancelled"
        } else if error.is::<StartupTimeout>() {
            "startup_timeout"
        } else {
            "startup_failed"
        });
        // A recording failure must not hide a process/stop/deadline failure.
        if let Err(record_error) = record(&progress) {
            return Err(error.context(format!("startup evidence save failed: {record_error}")));
        }
        return Err(match last_detail {
            Some(detail) if error.is::<StartupTimeout>() => {
                error.context(format!("startup_timeout: {detail}"))
            }
            _ => error,
        });
    }
    Ok(progress)
}

/// Loopback only; redirects, proxies and response bodies cannot make a
/// preparation signal escape its declared endpoint.
pub fn http_probe(
    client: &reqwest::blocking::Client,
    url: &str,
    budget: Duration,
) -> Result<ReadinessObservation> {
    match client.get(url).timeout(budget).send() {
        Ok(response) => Ok(ReadinessObservation {
            ready: response.status().is_success(),
            http_status: Some(response.status().as_u16()),
            detail: Some(format!("HTTP {}", response.status().as_u16())),
        }),
        Err(_) => Ok(ReadinessObservation {
            ready: false,
            http_status: None,
            detail: Some("loopback request failed".into()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[test]
    fn a_503_is_http_liveness_and_only_a_later_200_is_ready() {
        let mut statuses = [503, 200].into_iter();
        let phases = RefCell::new(Vec::new());
        let progress = wait_for_readiness(
            Duration::from_secs(1),
            |_| {
                let status = statuses.next().unwrap();
                Ok(ReadinessObservation {
                    ready: status == 200,
                    http_status: Some(status),
                    detail: None,
                })
            },
            || Ok(()),
            |p| {
                phases.borrow_mut().push(p.phase);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(progress.phase, StartupPhase::Ready);
        assert_eq!(
            *phases.borrow(),
            [
                StartupPhase::ProcessStarted,
                StartupPhase::HttpResponding,
                StartupPhase::HttpResponding,
                StartupPhase::Ready
            ]
        );
    }
    #[test]
    fn cancellation_during_a_successful_probe_cannot_publish_ready() {
        let calls = std::cell::Cell::new(0);
        let last = RefCell::new(None);
        let result = wait_for_readiness(
            Duration::from_secs(1),
            |_| {
                Ok(ReadinessObservation {
                    ready: true,
                    http_status: Some(200),
                    detail: None,
                })
            },
            || {
                calls.set(calls.get() + 1);
                if calls.get() > 1 {
                    return Err(StartupCancelled.into());
                }
                Ok(())
            },
            |p| {
                *last.borrow_mut() = Some(p.phase);
                Ok(())
            },
        );
        assert!(result.unwrap_err().is::<StartupCancelled>());
        assert_eq!(*last.borrow(), Some(StartupPhase::Cancelled));
    }
    #[test]
    fn exited_engine_is_not_relabelled_as_a_timeout() {
        let last = RefCell::new(None);
        let error = wait_for_readiness(
            Duration::from_secs(180),
            |_| unreachable!(),
            || anyhow::bail!("engine exited: 7"),
            |p| {
                *last.borrow_mut() = Some(p.phase);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("engine exited: 7"));
        assert_eq!(*last.borrow(), Some(StartupPhase::Failed));
    }
    #[test]
    fn an_expired_deadline_never_accepts_late_ready() {
        let last = RefCell::new(None);
        let error = wait_for_readiness(
            Duration::ZERO,
            |_| unreachable!(),
            || Ok(()),
            |p| {
                *last.borrow_mut() = Some(p.phase);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "startup_timeout");
        assert_eq!(*last.borrow(), Some(StartupPhase::Failed));
    }
    #[test]
    fn a_successful_probe_after_the_deadline_is_rejected_with_timeout_evidence() {
        let last = RefCell::new(None);
        let error = wait_for_readiness(
            Duration::from_millis(5),
            |_| {
                std::thread::sleep(Duration::from_millis(10));
                Ok(ReadinessObservation {
                    ready: true,
                    http_status: Some(200),
                    detail: None,
                })
            },
            || Ok(()),
            |p| {
                *last.borrow_mut() = Some((p.phase, p.failure));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.is::<StartupTimeout>());
        assert_eq!(
            *last.borrow(),
            Some((StartupPhase::Failed, Some("startup_timeout")))
        );
    }
}
