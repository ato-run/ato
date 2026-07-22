//! Best-effort build-time screenshot capture (store thumbnail automation).
//!
//! Shells out to a headless Chromium-family browser to capture a PNG of the
//! booted guest's root page during `FirecrackerBackend::build_ready_state`'s
//! warmup window (right after `wait_health` confirms the guest is live and
//! reachable — see `firecracker.rs`). The base64 result is threaded through
//! `BuildReadyStateReceipt::screenshot_png_base64` into the snapshot-builder's
//! ack body (`screenshot_png_base64`), which the ato-api ack handler decodes,
//! uploads to R2, and records as the capsule's store thumbnail — but only when
//! the capsule has no thumbnail yet, and never in a way that can fail the ack.
//!
//! **NEVER fails the build.** Every failure mode here (binary missing, guest
//! unreachable, timeout, oversized/garbled output) is logged with `eprintln!`
//! (matching this crate's existing logging convention — no `tracing`/`log`
//! dependency) and mapped to `None`. Callers must never propagate an error out
//! of this module with `?`; every public function here returns a plain value,
//! never a `Result`.

use std::net::SocketAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

/// Env var to force a specific headless-browser binary (absolute path or a
/// PATH-resolvable name). When unset (the common case), a fixed list of
/// common Chromium-family binary names is probed on PATH instead.
const BROWSER_BIN_ENV: &str = "ATO_SCREENSHOT_BROWSER_BIN";

/// Common headless-capable Chromium-family binary names, probed in order when
/// `ATO_SCREENSHOT_BROWSER_BIN` is unset.
const CANDIDATE_BINARIES: &[&str] = &["chromium", "chromium-browser", "google-chrome", "chrome"];

/// Bounded wall-clock budget for one capture attempt (process launch +
/// navigate + screenshot). Chosen to stay well inside a builder's boot
/// timeout — a screenshot is a nice-to-have, never worth stalling a build for.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(8);

/// Reject a captured PNG bigger than this many raw bytes. The ato-api ack
/// endpoint caps the base64 payload at 700_000 chars (~525_000 raw bytes) and
/// would just ignore an oversized one server-side; failing fast here avoids
/// wasting the base64 encode + the ack upload on a payload that would be
/// discarded anyway.
const MAX_PNG_BYTES: u64 = 500_000;

/// The 8-byte PNG file signature — a cheap sanity check that the headless
/// browser actually wrote a PNG (rather than, say, an empty file or an error
/// page it was told to screenshot as HTML).
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Best-effort: capture a PNG screenshot of `http://{addr}/` via a headless
/// Chromium-family browser and return it base64-encoded (STANDARD alphabet),
/// or `None` on ANY failure. Never blocks longer than `CAPTURE_TIMEOUT`; never
/// panics; never turns a build failure into anything worse than "no
/// screenshot".
pub fn capture_best_effort(addr: SocketAddr) -> Option<String> {
    let bin = resolve_browser_binary()?;

    // A dedicated, PID-scoped scratch dir: easy to fully clean up afterwards
    // (`remove_dir_all`) regardless of whether the browser wrote its output,
    // a lockfile, or a crash dump into it.
    let dir = std::env::temp_dir().join(format!("ato-screenshot-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[screenshot] skip: cannot create scratch dir {dir:?}: {e}");
        return None;
    }
    let out_path = dir.join("shot.png");
    let url = format!("http://{addr}/");

    let encoded = match run_capture(&bin, &out_path, &url) {
        Ok(()) => encode_if_within_cap(&out_path),
        Err(e) => {
            eprintln!("[screenshot] capture failed (best-effort, build unaffected): {e}");
            None
        }
    };
    // Always clean up, success or failure — never leak files in the builder's
    // work directory.
    let _ = std::fs::remove_dir_all(&dir);
    encoded
}

/// Resolve which browser binary to invoke, or `None` (logging once) when no
/// candidate is usable. `ATO_SCREENSHOT_BROWSER_BIN`, when set, is trusted
/// as an override — but only if it truly answers `--version`, so a stale/
/// misconfigured value degrades to "no screenshot" instead of a spawn error
/// bubbling up later.
fn resolve_browser_binary() -> Option<String> {
    if let Ok(explicit) = std::env::var(BROWSER_BIN_ENV) {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            if probe_binary(explicit) {
                return Some(explicit.to_string());
            }
            eprintln!("[screenshot] skip: {BROWSER_BIN_ENV}={explicit:?} did not answer --version");
            return None;
        }
    }
    for name in CANDIDATE_BINARIES {
        if probe_binary(name) {
            return Some((*name).to_string());
        }
    }
    eprintln!(
        "[screenshot] skip: no headless browser found on PATH (tried {CANDIDATE_BINARIES:?}); \
         set {BROWSER_BIN_ENV} to an explicit binary path to enable capture"
    );
    None
}

/// `<bin> --version` style probe: cheap, no window, no network. A binary that
/// cannot even answer `--version` cannot be trusted to screenshot.
fn probe_binary(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Run `<bin> --headless=new --screenshot=<out_path> ... <url>` bounded by
/// `CAPTURE_TIMEOUT`. No existing "command with a timeout" helper was found
/// elsewhere in the workspace (checked `docker_import`/`snapshot-builder`),
/// so this polls `Child::try_wait` — the same bounded-wait shape already used
/// for the guest health probe (`wait_health_until`) — rather than pulling in a
/// new dependency for one bounded subprocess call.
fn run_capture(bin: &str, out_path: &Path, url: &str) -> Result<(), String> {
    let mut child = Command::new(bin)
        .arg("--headless=new")
        .arg(format!("--screenshot={}", out_path.display()))
        .arg("--window-size=1280,800")
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn {bin}: {e}"))?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{bin} exited with {status}")),
            Ok(None) => {
                if start.elapsed() >= CAPTURE_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{bin} timed out after {CAPTURE_TIMEOUT:?}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait {bin}: {e}")),
        }
    }
}

/// Load `path`, enforce the size cap + PNG-magic sanity check, and base64
/// (STANDARD) encode it. `None` on any I/O error, an empty/oversized file, or
/// content that doesn't start with the PNG signature.
fn encode_if_within_cap(path: &Path) -> Option<String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[screenshot] skip: no output file at {path:?}: {e}");
            return None;
        }
    };
    if meta.len() == 0 {
        eprintln!("[screenshot] skip: empty output file at {path:?}");
        return None;
    }
    if meta.len() > MAX_PNG_BYTES {
        eprintln!(
            "[screenshot] skip: capture is {} bytes, over the {MAX_PNG_BYTES}-byte cap",
            meta.len()
        );
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[screenshot] skip: read {path:?}: {e}");
            return None;
        }
    };
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        eprintln!("[screenshot] skip: output at {path:?} is not a PNG");
        return None;
    }
    Some(BASE64.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_yields_none_without_panicking() {
        // ATO_SCREENSHOT_BROWSER_BIN pointed at a binary that cannot exist.
        // SAFETY: single-threaded test-local env mutation, restored immediately.
        unsafe {
            std::env::set_var(
                BROWSER_BIN_ENV,
                "/definitely/does/not/exist/ato-screenshot-test-binary",
            );
        }
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let result = capture_best_effort(addr);
        unsafe {
            std::env::remove_var(BROWSER_BIN_ENV);
        }
        assert!(result.is_none());
    }

    #[test]
    fn oversized_capture_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize((MAX_PNG_BYTES as usize) + 1, 0u8);
        std::fs::write(&path, &bytes).unwrap();
        assert!(encode_if_within_cap(&path).is_none());
    }

    #[test]
    fn non_png_output_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-png.png");
        std::fs::write(&path, b"<html>error</html>").unwrap();
        assert!(encode_if_within_cap(&path).is_none());
    }

    #[test]
    fn valid_small_png_is_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(b"fake-rest-of-png");
        std::fs::write(&path, &bytes).unwrap();
        let encoded = encode_if_within_cap(&path).expect("should encode");
        let decoded = BASE64.decode(encoded).unwrap();
        assert_eq!(decoded, bytes);
    }
}
