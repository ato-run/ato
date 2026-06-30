//! Desktop Runner — a *local* Ato Runner provider (#838).
//!
//! The Desktop Runner is the desktop shell acting as a Connected Runner backed
//! by a host isolation substrate. This module is the **capability model** (M0):
//! it probes the host, reports a structured [`DesktopRunnerFacts`], and decides
//! placement fail-closed ([`matching`]). It runs nothing for users yet — no
//! Ready-State restore, no CRIU, no binding injection, no snapshot serving (see
//! `docs/ready-state/desktop-runner.md` and `docs/ready-state/backend-matrix.md`).
//!
//! Architecture (M0 → future):
//! ```text
//! Desktop Runner -> macOS host -> Apple Containerization / `container`
//!   -> one lightweight Linux VM per session
//!   -> OCI container cold start            (M0)
//!   -> CRIU checkpoint inside the VM       (future, #839)
//!   -> BindingLease injection after start  (future)
//! ```
//!
//! Security invariants (enforced by the *absence* of capability in M0, asserted
//! by tests, and documented for the future path):
//! - A Desktop Runner is a Runner, not a Snapshot Server / Capsule Registry; the
//!   latter never executes user-bound sessions.
//! - Bindings are injected only after restore/start, never during build/seal,
//!   and binding values never touch CapsuleFS / ReadyStateManifest / rootfs /
//!   memory / vmstate / CRIU images / logs.
//! - Session isolation is per VM (VM-wrapped container); shared caches hold only
//!   immutable pre-bind artifacts.
//!
//! M0 is intentionally **not** wired into `ato run`/runner selection yet (that is
//! a #838 follow-up), so its placement/receipt surface is reachable only from
//! tests for now — hence the module-scoped `dead_code` allow.
#![allow(dead_code)]

pub(crate) mod facts;
pub(crate) mod macos;
pub(crate) mod matching;

use facts::{DesktopRunnerFacts, Maturity, PROVIDER_KIND_DESKTOP, SubstrateCapability};

/// WSL2 substrate placeholder identifier (Windows; not implemented in M0).
const SUBSTRATE_WSL2: &str = "wsl2";

/// The Desktop Runner runtime version (the `ato` CLI driving the runner).
fn runtime_version() -> &'static str {
    crate::application::runner_agent::agent_version()
}

/// Probe the live host and report its Desktop Runner capability facts.
///
/// Dispatches by `std::env::consts::OS`: macOS gets the Apple Containerization
/// probe; every other OS reports honest facts with **no** macOS capability
/// (Linux notes the separate Firecracker/KVM path; Windows carries a WSL2
/// placeholder). Never errors and never has side effects.
pub(crate) fn probe() -> DesktopRunnerFacts {
    match std::env::consts::OS {
        "macos" => macos::probe(runtime_version()),
        other => build_other_facts(other, std::env::consts::ARCH),
    }
}

/// Facts for a non-macOS host: honest, capability-free for Apple Containerization.
///
/// Linux Ready-State is the existing Firecracker/KVM runner path and is
/// deliberately **not** advertised here (it "remains separate" from the Desktop
/// Runner substrate). Windows carries a WSL2 placeholder only.
fn build_other_facts(host_os: &str, host_arch: &str) -> DesktopRunnerFacts {
    let mut substrates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut virtualization_available = false;

    match host_os {
        "linux" => {
            virtualization_available = std::path::Path::new("/dev/kvm").exists();
            diagnostics.push(
                "Desktop Runner Apple Containerization is macOS-only. On Linux, local Ready-State \
                 uses the separate Firecracker/KVM runner path."
                    .to_string(),
            );
        }
        "windows" => {
            substrates.push(SubstrateCapability {
                substrate: SUBSTRATE_WSL2.into(),
                available: false,
                tool: None,
                tool_path: None,
                tool_version: None,
                system_service_running: false,
                accelerator: None,
                maturity: Maturity::Experimental,
            });
            diagnostics.push(
                "Desktop Runner on Windows is a WSL2 placeholder (not implemented). Use a managed \
                 runner for local-equivalent execution."
                    .to_string(),
            );
        }
        _ => {
            diagnostics.push(format!(
                "Desktop Runner has no substrate for host OS '{host_os}'. Use a managed runner."
            ));
        }
    }

    DesktopRunnerFacts {
        provider_kind: PROVIDER_KIND_DESKTOP.into(),
        host_os: host_os.to_string(),
        host_arch: host_arch.to_string(),
        host_platform_version: None,
        desktop_runtime_version: runtime_version().to_string(),
        virtualization_available,
        substrates,
        backends: Vec::new(),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use facts::SUBSTRATE_APPLE_CONTAINERIZATION;

    #[test]
    fn probe_reports_this_host_without_panicking() {
        let facts = probe();
        assert_eq!(facts.provider_kind, PROVIDER_KIND_DESKTOP);
        assert_eq!(facts.host_os, std::env::consts::OS);
        // The receipt always serializes.
        assert!(facts.to_receipt_json().contains("provider_kind"));
    }

    #[test]
    fn linux_facts_keep_firecracker_capability_separate() {
        let facts = build_other_facts("linux", "x86_64");
        // The Desktop Runner module never claims Apple Containerization on Linux,
        // and never fabricates a firecracker backend — that path is separate.
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.backends.is_empty());
        assert!(facts.substrates.is_empty());
    }

    #[test]
    fn windows_facts_are_wsl2_placeholder_only_no_macos_capability() {
        let facts = build_other_facts("windows", "x86_64");
        assert!(!facts.has_substrate_backend(SUBSTRATE_APPLE_CONTAINERIZATION));
        assert!(facts.backends.is_empty());
        assert_eq!(facts.substrates.len(), 1);
        assert_eq!(facts.substrates[0].substrate, SUBSTRATE_WSL2);
        assert!(!facts.substrates[0].available);
    }
}

// ── Manual cold-OCI smoke (ignored) ────────────────────────────────────────
//
// Requires a real macOS 26 + Apple silicon host with Apple `container`
// installed and its system service running. Run explicitly:
//
//   cargo test -p cli desktop_runner::smoke -- --ignored --nocapture
//
// It pulls/runs a tiny HTTP OCI image, waits for it to answer, stops it, and
// prints a [`ColdOciSmokeReceipt`]. It never starts the `container` system
// service unless `ATO_DESKTOP_SMOKE_START_SERVICE=1` is set (opt-in).
#[cfg(test)]
mod smoke {
    use super::facts::SUBSTRATE_APPLE_CONTAINERIZATION;
    use serde::Serialize;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// Receipt emitted by the manual cold-OCI smoke.
    #[derive(Debug, Serialize)]
    struct ColdOciSmokeReceipt {
        host_os: String,
        host_arch: String,
        macos_version: Option<String>,
        container_version: Option<String>,
        substrate: String,
        isolation_boundary: String,
        ready_state_kind: String,
        elapsed_start_to_health_ms: u128,
    }

    /// The tiny HTTP image to cold-start. Overridable for offline mirrors.
    fn smoke_image() -> String {
        std::env::var("ATO_DESKTOP_SMOKE_IMAGE")
            .unwrap_or_else(|_| "docker.io/library/python:3-alpine".to_string())
    }

    #[test]
    #[ignore = "manual: needs macOS 26 + Apple silicon + `container`"]
    fn cold_oci_start_to_health() {
        let facts = super::probe();
        let Some(backend) = facts
            .backends
            .iter()
            .find(|b| b.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
        else {
            eprintln!(
                "SKIP: no Apple Containerization backend on this host:\n{}",
                facts.to_receipt_json()
            );
            return;
        };

        let service_running = facts
            .substrates
            .iter()
            .find(|s| s.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
            .is_some_and(|s| s.system_service_running);
        if !service_running {
            if std::env::var("ATO_DESKTOP_SMOKE_START_SERVICE").as_deref() == Ok("1") {
                let _ = Command::new("container").args(["system", "start"]).status();
            } else {
                eprintln!(
                    "SKIP: `container` system service not running; \
                     set ATO_DESKTOP_SMOKE_START_SERVICE=1 to opt into starting it."
                );
                return;
            }
        }

        let image = smoke_image();
        let name = "ato-desktop-smoke";
        // Best-effort cleanup of any prior run.
        let _ = Command::new("container").args(["stop", name]).status();

        let started = Instant::now();
        let run = Command::new("container")
            .args([
                "run",
                "-d",
                "--name",
                name,
                &image,
                "python3",
                "-m",
                "http.server",
                "8080",
            ])
            .status()
            .expect("spawn `container run`");
        assert!(run.success(), "container run failed for {image}");

        // Poll the container's health for up to 30s.
        let mut healthy = false;
        let deadline = started + Duration::from_secs(30);
        while Instant::now() < deadline {
            let ok = Command::new("container")
                .args([
                    "exec",
                    name,
                    "python3",
                    "-c",
                    "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080')",
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                healthy = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let elapsed = started.elapsed().as_millis();

        // Always stop the container, healthy or not.
        let _ = Command::new("container").args(["stop", name]).status();
        let _ = Command::new("container").args(["rm", name]).status();

        assert!(healthy, "{image} did not become healthy within 30s");

        let macos_version = facts.host_platform_version.clone();
        let container_version = facts
            .substrates
            .iter()
            .find(|s| s.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
            .and_then(|s| s.tool_version.clone());
        let receipt = ColdOciSmokeReceipt {
            host_os: facts.host_os.clone(),
            host_arch: facts.host_arch.clone(),
            macos_version,
            container_version,
            substrate: backend.substrate.clone(),
            isolation_boundary: serde_json::to_value(backend.isolation_boundary)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            ready_state_kind: serde_json::to_value(backend.ready_state_kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            elapsed_start_to_health_ms: elapsed,
        };
        println!(
            "cold-OCI smoke receipt:\n{}",
            serde_json::to_string_pretty(&receipt).unwrap()
        );
    }
}
