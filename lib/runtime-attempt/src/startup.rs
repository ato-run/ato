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

#[derive(Debug)]
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
/// A Runner advertising this can evaluate a JSON condition over a bounded
/// startup response. Without it the condition would be silently ignored and an
/// application still loading would be treated as started.
pub const STARTUP_JSON_RUNTIME_FEATURE: &str = "runtime_feature=startup_json_v1";
/// A Runner advertising this honours an exact startup status instead of
/// accepting any 2xx.
pub const STARTUP_STATUS_RUNTIME_FEATURE: &str = "runtime_feature=startup_expect_status_v1";

/// A physical condition on one startup response.
///
/// This is a preparation signal, never a Contract observation: it decides when
/// a Run may be treated as started, not whether K is satisfied.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProbeCondition {
    /// The exact status that counts as started; `None` means any 2xx.
    pub expect_status: Option<u16>,
    /// A condition on the response body.
    pub json: Option<JsonCondition>,
    /// How many body bytes may be read to evaluate `json`.
    pub max_body_bytes: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JsonCondition {
    /// RFC 6901 JSON Pointer; empty names the whole body.
    pub pointer: String,
    /// At least this integer.
    pub gte: Option<JsonScalar>,
    /// Exactly this value, including its JSON type.
    pub equals: Option<JsonScalar>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonScalar {
    Bool(bool),
    Integer(i64),
    Text(String),
}

impl JsonCondition {
    /// `Ok(false)` means "not yet", not "refuse": an application that answers
    /// before it finished booting is expected to report its own progress.
    fn satisfied(&self, body: &[u8]) -> Result<bool, String> {
        let Ok(document) = serde_json::from_slice::<serde_json::Value>(body) else {
            return Ok(false);
        };
        let Some(value) = document.pointer(&self.pointer) else {
            return Ok(false);
        };
        match (&self.gte, &self.equals) {
            (Some(threshold), None) => {
                let expected =
                    json_i64(threshold).ok_or_else(|| "a minimum must be an integer".to_owned())?;
                Ok(value.as_i64().is_some_and(|found| found >= expected))
            }
            (None, Some(expected)) => Ok(match expected {
                JsonScalar::Bool(expected) => value.as_bool() == Some(*expected),
                JsonScalar::Integer(expected) => value.as_i64() == Some(*expected),
                JsonScalar::Text(expected) => value.as_str() == Some(expected.as_str()),
            }),
            _ => Err("a startup condition states exactly one comparison".to_owned()),
        }
    }
}

fn json_i64(scalar: &JsonScalar) -> Option<i64> {
    match scalar {
        JsonScalar::Integer(value) => Some(*value),
        JsonScalar::Bool(_) | JsonScalar::Text(_) => None,
    }
}

/// The failure a probe reports when the body outgrew its declared bound, or a
/// condition it was handed is malformed. Both are refusals, not "wait".
fn condition_failure(detail: String) -> anyhow::Error {
    anyhow::anyhow!("startup_probe_failed: {detail}")
}

impl From<&ato_formation::requirements::StartupRequirement> for ProbeCondition {
    fn from(requirement: &ato_formation::requirements::StartupRequirement) -> Self {
        Self {
            expect_status: requirement.expect_status,
            json: requirement.json.as_ref().map(JsonCondition::from),
            max_body_bytes: requirement.body_max_bytes(),
        }
    }
}

impl From<&ato_formation::requirements::StartupJsonCondition> for JsonCondition {
    fn from(condition: &ato_formation::requirements::StartupJsonCondition) -> Self {
        use ato_formation::requirements::StartupJsonScalar as Scalar;
        let scalar = |value: &Scalar| match value {
            Scalar::Bool(value) => JsonScalar::Bool(*value),
            Scalar::Integer(value) => JsonScalar::Integer(*value),
            Scalar::Text(value) => JsonScalar::Text(value.clone()),
        };
        Self {
            pointer: condition.pointer.clone(),
            gte: condition.gte.as_ref().map(scalar),
            equals: condition.equals.as_ref().map(scalar),
        }
    }
}

impl From<&ato_ipc::runtime_launch::ReadinessJsonConditionV1> for JsonCondition {
    fn from(condition: &ato_ipc::runtime_launch::ReadinessJsonConditionV1) -> Self {
        use ato_ipc::runtime_launch::ReadinessJsonScalarV1 as Scalar;
        let scalar = |value: &Scalar| match value {
            Scalar::Bool(value) => JsonScalar::Bool(*value),
            Scalar::Integer(value) => JsonScalar::Integer(*value),
            Scalar::Text(value) => JsonScalar::Text(value.clone()),
        };
        Self {
            pointer: condition.pointer.clone(),
            gte: condition.gte.as_ref().map(scalar),
            equals: condition.equals.as_ref().map(scalar),
        }
    }
}

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
    http_probe_with(client, url, budget, None)
}

/// Read at most `limit + 1` bytes so an overrun is detectable without ever
/// buffering more than the declared bound.
fn read_bounded(response: reqwest::blocking::Response, limit: u32) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut body = Vec::new();
    response
        .take(u64::from(limit) + 1)
        .read_to_end(&mut body)
        .map_err(|error| condition_failure(format!("cannot read the response body: {error}")))?;
    if body.len() > limit as usize {
        return Err(condition_failure(format!(
            "response body exceeded the declared {limit} bytes"
        )));
    }
    Ok(body)
}

/// A probe that also judges an exact status and a bounded JSON condition.
///
/// A status that is not the expected one, a pointer that does not resolve and a
/// value that is short of its threshold all mean "not started yet": they are
/// reported as not-ready so the caller keeps polling within its deadline. A
/// body that outgrows its declared bound, or a malformed condition, is a
/// refusal — waiting longer cannot make it true.
pub fn http_probe_with(
    client: &reqwest::blocking::Client,
    url: &str,
    budget: Duration,
    condition: Option<&ProbeCondition>,
) -> Result<ReadinessObservation> {
    let response = match client.get(url).timeout(budget).send() {
        Ok(response) => response,
        Err(_) => {
            return Ok(ReadinessObservation {
                ready: false,
                http_status: None,
                detail: Some("loopback request failed".into()),
            });
        }
    };
    let status = response.status().as_u16();
    let Some(condition) = condition else {
        return Ok(ReadinessObservation {
            ready: response.status().is_success(),
            http_status: Some(status),
            detail: Some(format!("HTTP {status}")),
        });
    };
    let status_ready = condition
        .expect_status
        .map_or(response.status().is_success(), |expect| expect == status);
    if !status_ready {
        return Ok(ReadinessObservation {
            ready: false,
            http_status: Some(status),
            detail: Some(match condition.expect_status {
                Some(expect) => format!("HTTP {status} while expecting {expect}"),
                None => format!("HTTP {status}"),
            }),
        });
    }
    let Some(json) = &condition.json else {
        return Ok(ReadinessObservation {
            ready: true,
            http_status: Some(status),
            detail: Some(format!("HTTP {status}")),
        });
    };
    let body = read_bounded(response, condition.max_body_bytes)?;
    let satisfied = json.satisfied(&body).map_err(condition_failure)?;
    Ok(ReadinessObservation {
        ready: satisfied,
        http_status: Some(status),
        detail: Some(if satisfied {
            format!("HTTP {status} and {} holds", json.pointer)
        } else {
            format!("HTTP {status} but {} does not hold yet", json.pointer)
        }),
    })
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
    /// A loopback server answering one canned response per accepted connection.
    fn probe_server(responses: Vec<String>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/progress", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0_u8; 2048];
                let _ = std::io::Read::read(&mut stream, &mut buffer);
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        url
    }

    fn http_response(status: u16, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn probe_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    fn json_gte(pointer: &str, threshold: i64) -> ProbeCondition {
        ProbeCondition {
            expect_status: None,
            json: Some(JsonCondition {
                pointer: pointer.to_owned(),
                gte: Some(JsonScalar::Integer(threshold)),
                equals: None,
            }),
            max_body_bytes: 4096,
        }
    }

    #[test]
    fn q52_an_expect_status_mismatch_keeps_waiting_and_finally_fails() {
        let url = probe_server(vec![
            http_response(200, "{}"),
            http_response(200, "{}"),
            http_response(200, "{}"),
        ]);
        let client = probe_client();
        let condition = ProbeCondition {
            expect_status: Some(204),
            ..Default::default()
        };
        let observations = RefCell::new(Vec::new());
        let result = wait_for_readiness(
            Duration::from_millis(250),
            |budget| {
                let observation = http_probe_with(&client, &url, budget, Some(&condition))?;
                observations.borrow_mut().push(observation.ready);
                Ok(observation)
            },
            || Ok(()),
            |_| Ok(()),
        );
        let error = result.unwrap_err();
        assert!(error.is::<StartupTimeout>(), "{error:#}");
        // Every 200 was evidence of life, never of readiness.
        assert!(
            observations.borrow().iter().all(|ready| !ready),
            "a 200 satisfied an expectation of 204"
        );
    }

    #[test]
    fn q52_a_json_threshold_shortfall_waits_and_a_reached_threshold_succeeds() {
        let url = probe_server(vec![
            http_response(200, "{\"data\":{\"progress\":40}}"),
            http_response(200, "{\"data\":{\"progress\":99}}"),
            http_response(200, "{\"data\":{\"progress\":100}}"),
        ]);
        let client = probe_client();
        let condition = json_gte("/data/progress", 100);
        let progress = wait_for_readiness(
            Duration::from_secs(5),
            |budget| http_probe_with(&client, &url, budget, Some(&condition)),
            || Ok(()),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(progress.phase, StartupPhase::Ready);
        assert_eq!(progress.last_http_status, Some(200));
    }

    #[test]
    fn q52_a_missing_pointer_is_not_yet_ready_rather_than_a_refusal() {
        let url = probe_server(vec![
            http_response(200, "{\"data\":{}}"),
            http_response(200, "{\"data\":{\"progress\":100}}"),
        ]);
        let client = probe_client();
        let condition = json_gte("/data/progress", 100);
        let first =
            http_probe_with(&client, &url, Duration::from_secs(1), Some(&condition)).unwrap();
        assert!(!first.ready);
        assert_eq!(first.http_status, Some(200));
        let second =
            http_probe_with(&client, &url, Duration::from_secs(1), Some(&condition)).unwrap();
        assert!(second.ready);
    }

    #[test]
    fn q52_an_oversized_body_is_a_refusal_not_a_longer_wait() {
        let body = format!(
            "{{\"data\":{{\"progress\":100,\"pad\":\"{}\"}}}}",
            "x".repeat(2048)
        );
        let url = probe_server(vec![http_response(200, &body)]);
        let client = probe_client();
        let mut condition = json_gte("/data/progress", 100);
        condition.max_body_bytes = 256;
        let error =
            http_probe_with(&client, &url, Duration::from_secs(1), Some(&condition)).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("exceeded"), "{message}");
        assert!(!error.is::<StartupTimeout>(), "{message}");
    }

    #[test]
    fn q52_equals_compares_the_declared_json_type() {
        let url = probe_server(vec![
            http_response(200, "{\"ready\":\"true\"}"),
            http_response(200, "{\"ready\":true}"),
        ]);
        let client = probe_client();
        let condition = ProbeCondition {
            expect_status: Some(200),
            json: Some(JsonCondition {
                pointer: "/ready".to_owned(),
                gte: None,
                equals: Some(JsonScalar::Bool(true)),
            }),
            max_body_bytes: 1024,
        };
        // The string "true" is not the boolean true.
        assert!(
            !http_probe_with(&client, &url, Duration::from_secs(1), Some(&condition))
                .unwrap()
                .ready
        );
        assert!(
            http_probe_with(&client, &url, Duration::from_secs(1), Some(&condition))
                .unwrap()
                .ready
        );
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
