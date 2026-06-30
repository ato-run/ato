//! Apple Containerization port-publish / serving **verification** (#838, MacBook
//! M3.5 — Step 1).
//!
//! Local serving (publishing a capsule's port so the user can reach it from the
//! host) must not be wired into the live `ato run` path on guessed CLI syntax.
//! This module is the *verification harness*: a hardware-gated, ignored smoke
//! that, on a real macOS 26 Apple-silicon host with Apple `container`, discovers
//! the actual publish/stop command shape and proves a published port is
//! reachable from the host — emitting a [`PortVerificationReceipt`] that Step 2
//! (live serving) will be implemented against.
//!
//! The whole module is `#[cfg(test)]`: nothing here changes the product. The
//! live cold-OCI path (`cold_oci::run`) still only checks running-state and
//! tears down — it never publishes a port. See
//! `docs/ready-state/desktop-runner-port-verification.md` for the runbook.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::cold_oci::{ContainerGuard, container};
use super::facts::SUBSTRATE_APPLE_CONTAINERIZATION;

/// Publish-flag candidates to trial, in order. Apple `container` most likely
/// mirrors Docker/Podman (`--publish` / `-p`); the smoke confirms which works.
const PUBLISH_FLAG_CANDIDATES: &[&str] = &["--publish", "-p"];

/// Allocate an available local TCP port by binding `127.0.0.1:0` and reading the
/// OS-assigned port. Best-effort (a tiny TOCTOU window before the container
/// republishes it); fine for a manual smoke.
pub(crate) fn allocate_local_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Build the publish argument pair for a `host:guest` mapping (pure).
/// Empty when there is no declared guest port — no publish without a port.
pub(crate) fn publish_args(flag: &str, host_port: u16, guest_port: Option<u16>) -> Vec<String> {
    match guest_port {
        Some(guest) => vec![flag.to_string(), format!("{host_port}:{guest}")],
        None => Vec::new(),
    }
}

/// The Step-1 verification receipt: the real CLI shape Step 2 will rely on.
#[derive(Debug, Clone, Serialize)]
struct PortVerificationReceipt {
    host_os: String,
    host_arch: String,
    macos_version: Option<String>,
    container_version: Option<String>,
    image: String,
    guest_port: u16,
    host_port: u16,
    run_command_shape: String,
    /// The publish flag that actually worked (`--publish` / `-p`), or `None`.
    working_publish_flag: Option<String>,
    publish_command_shape: Option<String>,
    healthcheck_url: String,
    reachable: bool,
    start_to_health_ms: Option<u128>,
    cleanup_ok: bool,
    second_stop_safe: bool,
}

/// A unique container name for one verification attempt.
fn attempt_name() -> String {
    let pid = std::process::id();
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("ato-desktop-portverify-{pid}-{millis}")
}

/// TCP-connect to `127.0.0.1:port`, polling until reachable or the deadline.
/// A successful connect proves the published port is listening on the host.
fn wait_reachable(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(500),
        )
        .is_ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// The tiny HTTP image to publish. Overridable for offline mirrors.
fn smoke_image() -> String {
    std::env::var("ATO_DESKTOP_SMOKE_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/python:3-alpine".to_string())
}

#[test]
#[ignore = "manual: needs macOS 26 + Apple silicon + `container` (M3.5 Step 1 — port publish verification)"]
fn port_serving_verification() {
    let facts = super::probe();
    let Some(_backend) = facts
        .backends
        .iter()
        .find(|b| b.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
    else {
        eprintln!("SKIP: no Apple Containerization backend on this host");
        return;
    };

    let service_running = facts
        .substrates
        .iter()
        .find(|s| s.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
        .is_some_and(|s| s.system_service_running);
    if !service_running && std::env::var("ATO_DESKTOP_SMOKE_START_SERVICE").as_deref() == Ok("1") {
        let _ = container(&["system", "start"], Duration::from_secs(30));
    } else if !service_running {
        eprintln!(
            "SKIP: `container` system service not running; \
             set ATO_DESKTOP_SMOKE_START_SERVICE=1 to opt into starting it."
        );
        return;
    }

    let image = smoke_image();
    let guest_port: u16 = 8080;
    let guest_str = guest_port.to_string();

    let mut working_flag: Option<String> = None;
    let mut publish_shape: Option<String> = None;
    let mut run_shape = String::new();
    let mut reachable = false;
    let mut start_to_health_ms: Option<u128> = None;
    let mut host_port: u16 = 0;
    let mut cleanup_ok = false;
    let mut second_stop_safe = false;

    // Trial each publish-flag candidate until one runs AND its port is reachable.
    for flag in PUBLISH_FLAG_CANDIDATES {
        let port = match allocate_local_port() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("port alloc failed: {e}");
                continue;
            }
        };
        host_port = port;
        let pub_pair = publish_args(flag, port, Some(guest_port));
        let name = attempt_name();
        let mut guard = ContainerGuard::new(name.clone());

        let run_args: Vec<&str> = vec![
            "run",
            "-d",
            "--name",
            name.as_str(),
            pub_pair[0].as_str(),
            pub_pair[1].as_str(),
            image.as_str(),
            "python3",
            "-m",
            "http.server",
            guest_str.as_str(),
        ];
        run_shape = format!("container {}", run_args.join(" "));

        let started = Instant::now();
        let run = container(&run_args, Duration::from_secs(120));
        if !run.status_ok {
            eprintln!(
                "publish flag {flag} did not run (timed_out={}): {}",
                run.timed_out,
                run.stderr.trim()
            );
            guard.cleanup();
            continue;
        }

        if wait_reachable(port, Duration::from_secs(30)) {
            reachable = true;
            start_to_health_ms = Some(started.elapsed().as_millis());
            working_flag = Some((*flag).to_string());
            publish_shape = Some(format!("{flag} {host_port}:{guest_port}"));
            // Stop + delete the container.
            cleanup_ok = guard.cleanup();
            // Double-stop safety: re-issue `container stop` on the now-gone
            // container directly (bypassing the guard's idempotency flag). "Safe"
            // = the CLI returns promptly without hanging, whatever its exit code.
            second_stop_safe = !container(&["stop", &name], Duration::from_secs(15)).timed_out;
            break;
        }

        eprintln!("publish flag {flag} ran but port {port} was not reachable");
        guard.cleanup();
    }

    let container_version = facts
        .substrates
        .iter()
        .find(|s| s.substrate == SUBSTRATE_APPLE_CONTAINERIZATION)
        .and_then(|s| s.tool_version.clone());

    let receipt = PortVerificationReceipt {
        host_os: facts.host_os.clone(),
        host_arch: facts.host_arch.clone(),
        macos_version: facts.host_platform_version.clone(),
        container_version,
        image: image.clone(),
        guest_port,
        host_port,
        run_command_shape: run_shape,
        working_publish_flag: working_flag,
        publish_command_shape: publish_shape,
        healthcheck_url: format!("http://127.0.0.1:{host_port}"),
        reachable,
        start_to_health_ms,
        cleanup_ok,
        second_stop_safe,
    };
    println!(
        "port-verification receipt:\n{}",
        serde_json::to_string_pretty(&receipt).unwrap()
    );

    assert!(
        receipt.reachable,
        "no publish flag produced a host-reachable port — record the receipt and adjust \
         PUBLISH_FLAG_CANDIDATES / run shape from the real `container` CLI on macOS 26"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_args_built_only_from_declared_port() {
        assert_eq!(
            publish_args("--publish", 54321, Some(8080)),
            vec!["--publish".to_string(), "54321:8080".to_string()]
        );
        assert_eq!(
            publish_args("-p", 1, Some(2)),
            vec!["-p".to_string(), "1:2".to_string()]
        );
    }

    #[test]
    fn no_publish_without_a_guest_port() {
        assert!(publish_args("--publish", 54321, None).is_empty());
    }

    #[test]
    fn allocate_local_port_returns_a_nonzero_ephemeral_port() {
        let p = allocate_local_port().expect("allocate");
        assert!(p >= 1024, "expected an ephemeral port, got {p}");
        // Two allocations should each succeed (the OS may or may not reuse a
        // port — we only require a valid, non-zero port, not uniqueness).
        let q = allocate_local_port().expect("allocate again");
        assert!(q >= 1024, "{q}");
    }

    #[test]
    fn receipt_serializes_with_verification_fields() {
        let r = PortVerificationReceipt {
            host_os: "macos".into(),
            host_arch: "aarch64".into(),
            macos_version: Some("26.0".into()),
            container_version: Some("container 0.1.0".into()),
            image: "img".into(),
            guest_port: 8080,
            host_port: 54321,
            run_command_shape: "container run ...".into(),
            working_publish_flag: Some("--publish".into()),
            publish_command_shape: Some("--publish 54321:8080".into()),
            healthcheck_url: "http://127.0.0.1:54321".into(),
            reachable: true,
            start_to_health_ms: Some(1234),
            cleanup_ok: true,
            second_stop_safe: true,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains("\"working_publish_flag\":\"--publish\""),
            "{json}"
        );
        assert!(json.contains("\"reachable\":true"), "{json}");
        assert!(json.contains("\"second_stop_safe\":true"), "{json}");
    }
}
