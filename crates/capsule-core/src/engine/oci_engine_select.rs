//! Shared OCI engine (docker/podman) selection.
//!
//! Centralizes the policy that picks the container engine from the
//! `container_runtime` config field, so the executor and packer call sites
//! cannot diverge. Guarantees, versus the original `which`-only probe:
//!
//! * An explicitly configured runtime is never silently overridden, and the
//!   *other* engine is never probed — a stopped Docker must not slow a
//!   `container_runtime=podman` launch (and vice versa).
//! * Health probes are bounded by a timeout, so a hung or unhealthy daemon (a
//!   common Windows Docker-Desktop / Podman-machine state) cannot wedge a
//!   launch indefinitely.
//! * An unknown `container_runtime` value is a hard error, not a silent
//!   fall-through to auto-selection (so a typo like `"dockre"` is caught).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{CapsuleError, Result};

/// Bounded per-engine health probe so an unresponsive daemon can't stall a
/// launch. Overridable via `ATO_OCI_HEALTH_TIMEOUT_SECS` for slow hosts.
fn health_probe_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("ATO_OCI_HEALTH_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .unwrap_or(5),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OciEngineKind {
    Docker,
    Podman,
}

impl OciEngineKind {
    pub fn binary(self) -> &'static str {
        match self {
            OciEngineKind::Docker => "docker",
            OciEngineKind::Podman => "podman",
        }
    }
}

/// Select the OCI engine honoring the `container_runtime` config field.
pub fn select_oci_engine() -> Result<OciEngineKind> {
    let configured = crate::config::load_config()
        .unwrap_or_default()
        .container_runtime;
    let engine = decide_engine(configured.as_deref(), runtime_healthy)?;
    tracing::info!(
        engine = ?engine,
        configured = configured.as_deref().unwrap_or("auto"),
        "selected OCI engine"
    );
    Ok(engine)
}

/// Pure selection policy, split out so it can be unit-tested without a real
/// docker/podman install. `healthy` probes a given engine binary; it is only
/// called for the engines this policy actually needs, so callers can rely on
/// "configured runtime ⇒ the other engine is never probed".
fn decide_engine(
    configured: Option<&str>,
    mut healthy: impl FnMut(&str) -> bool,
) -> Result<OciEngineKind> {
    match configured {
        Some("docker") => require_healthy(OciEngineKind::Docker, &mut healthy),
        Some("podman") => require_healthy(OciEngineKind::Podman, &mut healthy),
        None | Some("auto") => {
            // Probe in preference order and short-circuit, so a present docker
            // is not slowed by also probing podman.
            if healthy("docker") {
                Ok(OciEngineKind::Docker)
            } else if healthy("podman") {
                Ok(OciEngineKind::Podman)
            } else {
                Err(CapsuleError::ContainerEngine(
                    "no healthy container runtime found: docker and podman are both \
                     unavailable or unhealthy"
                        .to_string(),
                ))
            }
        }
        Some(other) => Err(CapsuleError::ContainerEngine(format!(
            "invalid container_runtime = {other:?}; expected \"auto\", \"docker\", or \"podman\""
        ))),
    }
}

fn require_healthy(
    engine: OciEngineKind,
    healthy: &mut impl FnMut(&str) -> bool,
) -> Result<OciEngineKind> {
    if healthy(engine.binary()) {
        Ok(engine)
    } else {
        Err(CapsuleError::ContainerEngine(format!(
            "configured container_runtime = {:?} but {} is not available or not healthy",
            engine.binary(),
            engine.binary()
        )))
    }
}

/// Run `<binary> info` with a bounded wait. Returns false on spawn failure,
/// non-zero exit, or timeout (the child is killed so it cannot linger).
fn runtime_healthy(binary: &str) -> bool {
    let mut child = match Command::new(binary)
        .args(["info", "--format", "ok"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let deadline = Instant::now() + health_probe_timeout();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn configured_docker_selects_docker_without_probing_podman() {
        let calls = RefCell::new(Vec::new());
        let got = decide_engine(Some("docker"), |b| {
            calls.borrow_mut().push(b.to_string());
            b == "docker"
        })
        .unwrap();
        assert_eq!(got, OciEngineKind::Docker);
        assert_eq!(*calls.borrow(), vec!["docker".to_string()]);
    }

    #[test]
    fn configured_podman_selects_podman_without_probing_docker() {
        let calls = RefCell::new(Vec::new());
        let got = decide_engine(Some("podman"), |b| {
            calls.borrow_mut().push(b.to_string());
            b == "podman"
        })
        .unwrap();
        assert_eq!(got, OciEngineKind::Podman);
        assert_eq!(*calls.borrow(), vec!["podman".to_string()]);
    }

    #[test]
    fn configured_runtime_unhealthy_errors_and_does_not_fall_back() {
        // podman is healthy but docker is configured and down: must error, and
        // must not silently switch to podman.
        let calls = RefCell::new(Vec::new());
        let err = decide_engine(Some("docker"), |b| {
            calls.borrow_mut().push(b.to_string());
            b == "podman"
        })
        .unwrap_err();
        assert!(matches!(err, CapsuleError::ContainerEngine(_)));
        assert_eq!(*calls.borrow(), vec!["docker".to_string()]);
    }

    #[test]
    fn auto_prefers_docker_and_short_circuits() {
        let calls = RefCell::new(Vec::new());
        let got = decide_engine(None, |b| {
            calls.borrow_mut().push(b.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, OciEngineKind::Docker);
        assert_eq!(*calls.borrow(), vec!["docker".to_string()]);
    }

    #[test]
    fn auto_falls_back_to_podman_when_docker_down() {
        let got = decide_engine(Some("auto"), |b| b == "podman").unwrap();
        assert_eq!(got, OciEngineKind::Podman);
    }

    #[test]
    fn auto_errors_when_no_engine_healthy() {
        let err = decide_engine(None, |_| false).unwrap_err();
        assert!(matches!(err, CapsuleError::ContainerEngine(_)));
    }

    #[test]
    fn unknown_runtime_value_is_rejected_without_probing() {
        let calls = RefCell::new(Vec::new());
        let err = decide_engine(Some("containerd"), |b| {
            calls.borrow_mut().push(b.to_string());
            true
        })
        .unwrap_err();
        assert!(matches!(err, CapsuleError::ContainerEngine(_)));
        assert!(calls.borrow().is_empty());
    }
}
