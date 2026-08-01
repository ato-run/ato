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
//! # Threat model & confinement (why the browser is caged)
//!
//! The page being screenshotted is an **arbitrary, attacker-controlled**
//! capsule root page (store / GitHub-URL submissions are user-supplied), and
//! the browser runs on the **builder host**, which holds
//! `SNAPSHOT_BUILDER_AGENT_TOKEN` + `ATO_API_URL` and sits on the builder's
//! network. The screenshot is then **published as a public store thumbnail**.
//! Without confinement the page's JS could `fetch()` cloud metadata
//! (169.254.169.254), builder-host localhost services, or internal endpoints,
//! paint the response into its own DOM, and exfiltrate the secret into the
//! public thumbnail. The screenshot is therefore an attacker-controlled,
//! publicly-published output channel and MUST be caged.
//!
//! Two enforced controls, both kernel-level, established BEFORE the browser
//! starts and torn down when [`capture_best_effort`] returns:
//!
//! 1. **Egress lockdown (the exfil channel).** The browser runs as a dedicated
//!    non-root uid, and `iptables`/`ip6tables` `OUTPUT` rules — scoped to that
//!    uid via `-m owner --uid-owner` — permit exactly one destination
//!    (`tcp` to the single guest `addr:port`) and DROP everything else the uid
//!    emits: metadata, loopback (builder-host localhost services are
//!    explicitly in-scope, so loopback is NOT allowed), internal networks, the
//!    internet, and the entire other IP family. UDP is denied outright (no
//!    DNS/QUIC/WebRTC egress; the guest URL is a numeric IP so no DNS is
//!    needed). A fully compromised renderer still cannot reach anything but the
//!    guest it was already screenshotting.
//! 2. **Non-root browser.** Dropping root to that uid before `exec` means a
//!    renderer/browser exploit lands as an unprivileged, egress-locked uid —
//!    not as the (root) builder — with no ownership of any builder file.
//!
//! # FAIL CLOSED on confinement, best-effort on capture
//!
//! These are different axes and must not be blurred. If confinement cannot be
//! established, [`capture_best_effort`] returns `None` (no screenshot) and
//! NEVER falls back to an unconfined/root capture. "Best-effort" governs only
//! whether we *get* a screenshot (missing browser, unreachable guest, timeout,
//! oversized/garbled output → `None`), never *whether the browser is caged*.
//!
//! **NEVER fails the build.** Every failure mode here is logged with
//! `eprintln!` (matching this crate's existing logging convention — no
//! `tracing`/`log` dependency) and mapped to `None`. Callers must never
//! propagate an error out of this module with `?`; every public function here
//! returns a plain value, never a `Result`.

use std::net::SocketAddr;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::IpAddr;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Env var to force a specific headless-browser binary (absolute path or a
/// PATH-resolvable name). When unset (the common case), a fixed list of
/// common Chromium-family binary names is probed on PATH instead.
#[cfg(unix)]
const BROWSER_BIN_ENV: &str = "ATO_SCREENSHOT_BROWSER_BIN";

/// Common headless-capable Chromium-family binary names, probed in order when
/// `ATO_SCREENSHOT_BROWSER_BIN` is unset.
#[cfg(unix)]
const CANDIDATE_BINARIES: &[&str] = &["chromium", "chromium-browser", "google-chrome", "chrome"];

/// Env overrides for the firewall tooling (mainly for tests / unusual hosts).
#[cfg(unix)]
const IPTABLES_BIN_ENV: &str = "ATO_SCREENSHOT_IPTABLES_BIN";
#[cfg(unix)]
const IP6TABLES_BIN_ENV: &str = "ATO_SCREENSHOT_IP6TABLES_BIN";

/// Env override for the unprivileged uid the browser runs as. Must be non-root.
/// Defaults to a fixed uid in the dynamic range that no builder daemon is
/// expected to use — so the `-m owner --uid-owner` egress rules match ONLY our
/// browser and never collateral-block another process's traffic (which is why
/// `nobody`/65534 is deliberately NOT the default).
#[cfg(unix)]
const SANDBOX_UID_ENV: &str = "ATO_SCREENSHOT_UID";
#[cfg(unix)]
const DEFAULT_SANDBOX_UID: u32 = 61234;

/// Bounded wall-clock budget for one capture attempt (process launch +
/// navigate + screenshot). A restore-time capture runs while the builder is
/// still under heavy rootfs/CAS I/O, and large JavaScript apps can take longer
/// than eight seconds merely to start Chromium and paint their first frame.
/// Sixty seconds remains well inside the authoring lease while allowing that
/// required post-restore evidence to be produced reliably.
#[cfg(unix)]
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
/// Let client-rendered applications hydrate and paint after the initial
/// document load. Chrome's screenshot CLI does not wait for client rendering,
/// while `--virtual-time-budget` may never finish for apps with continuously
/// scheduled timers/workers (Swagger Editor, Monaco, etc.). Keep the same
/// browser alive over its local DevTools pipe and capture after this bounded
/// wall-clock delay instead. This remains strictly below [`CAPTURE_TIMEOUT`].
#[cfg(unix)]
const RENDER_SETTLE_BUDGET: Duration = Duration::from_secs(20);
/// Some applications defer their first meaningful paint until the target is
/// visible and an animation/intersection-observer frame has run. If the first
/// capture is still blank, keep the same caged browser alive for one bounded
/// retry instead of relaunching it into the same cold state.
#[cfg(unix)]
const RENDER_RETRY_BUDGET: Duration = Duration::from_secs(10);

#[cfg(unix)]
const MAX_CDP_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

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

/// Non-unix builders are unsupported: there is no confinement mechanism here,
/// so — fail closed — we never capture. (The real backend is Linux/KVM anyway.)
#[cfg(not(unix))]
pub fn capture_best_effort(_addr: SocketAddr) -> Option<String> {
    eprintln!(
        "[screenshot] skip: build-time screenshot capture is only supported on unix builders"
    );
    None
}

/// Best-effort: capture a PNG screenshot of `http://{addr}/` via a headless
/// Chromium-family browser, **caged** so the browser can reach only `addr`, and
/// return it base64-encoded (STANDARD alphabet), or `None` on ANY failure.
///
/// Fail-closed: if the cage ([`establish_confinement`]) cannot be built, this
/// returns `None` and the browser is never launched. Never blocks longer than
/// `CAPTURE_TIMEOUT`; never panics; never turns a build failure into anything
/// worse than "no screenshot".
#[cfg(unix)]
pub fn capture_best_effort(addr: SocketAddr) -> Option<String> {
    let bin = resolve_browser_binary()?;

    // FAIL CLOSED: no cage ⇒ no capture. The browser is NEVER run unconfined
    // against attacker-controlled capsule content. `confinement` holds the live
    // egress rules; dropping it (any return path below) tears them down.
    let confinement = establish_confinement(addr)?;

    // A dedicated, PID-scoped scratch dir: easy to fully clean up afterwards
    // (`remove_dir_all`) regardless of whether the browser wrote its output,
    // a lockfile, or a crash dump into it.
    let dir = std::env::temp_dir().join(format!("ato-screenshot-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[screenshot] skip: cannot create scratch dir {dir:?}: {e}");
        return None;
    }
    // The browser runs as `confinement.uid`; hand that uid ownership of its
    // scratch dir so it can write the PNG + its throwaway profile there.
    if let Err(e) = std::os::unix::fs::chown(&dir, Some(confinement.uid), Some(confinement.uid)) {
        eprintln!(
            "[screenshot] skip: cannot chown scratch dir {dir:?} to uid {}: {e}",
            confinement.uid
        );
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let out_path = dir.join("shot.png");
    let url = format!("http://{addr}/");

    let encoded = match run_capture(&bin, &out_path, &url, confinement.uid) {
        Ok(()) => encode_if_within_cap(&out_path),
        Err(e) => {
            eprintln!("[screenshot] capture failed (best-effort, build unaffected): {e}");
            None
        }
    };
    // Always clean up, success or failure — never leak files in the builder's
    // work directory. `confinement` drops at end of scope → egress rules removed.
    let _ = std::fs::remove_dir_all(&dir);
    encoded
}

/// Resolve which browser binary to invoke, or `None` (logging once) when no
/// candidate is usable. `ATO_SCREENSHOT_BROWSER_BIN`, when set, is trusted
/// as an override — but only if it truly answers `--version`, so a stale/
/// misconfigured value degrades to "no screenshot" instead of a spawn error
/// bubbling up later.
#[cfg(unix)]
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
#[cfg(unix)]
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

/// A live network cage for the screenshot browser: a dedicated unprivileged uid
/// plus kernel egress rules that permit ONLY tcp to the one guest addr. Its
/// `Drop` removes every rule it installed (on every path, incl. capture
/// failure / timeout / panic).
#[cfg(unix)]
struct Confinement {
    /// The unprivileged uid the browser (and all its children) run as.
    uid: u32,
    /// `(binary, delete-args)` for each `OUTPUT` rule we installed, replayed on
    /// `Drop`. Kept as owned argv (no shell) mirroring the backend's `ip`/
    /// `iptables` call style.
    teardown: Vec<(String, Vec<String>)>,
}

#[cfg(unix)]
impl Drop for Confinement {
    fn drop(&mut self) {
        for (bin, del) in &self.teardown {
            // Remove our rule(s). Loop: a spec may have been duplicated by a
            // crashed prior run plus our pre-clean; delete until none remain.
            for _ in 0..8 {
                if !run_ipt(bin, del) {
                    break;
                }
            }
        }
    }
}

/// Build the egress cage for `addr`, or `None` if it cannot be fully
/// established (⇒ the caller must NOT capture). Requires root/CAP_NET_ADMIN
/// (the builder runs as root) so it can both run the browser as a dedicated uid
/// and install `OUTPUT` rules. Every precondition that fails maps to `None`
/// (fail closed): not root, refusing uid 0, or any `iptables`/`ip6tables` rule
/// that will not install (tool missing, no CAP_NET_ADMIN, owner match absent).
#[cfg(unix)]
fn establish_confinement(addr: SocketAddr) -> Option<Confinement> {
    if !is_root() {
        eprintln!(
            "[screenshot] skip: confinement needs root/CAP_NET_ADMIN (to run the browser as a \
             dedicated uid and install egress rules); capturing nothing"
        );
        return None;
    }
    let uid = sandbox_uid();
    if uid == 0 {
        eprintln!(
            "[screenshot] skip: refusing to run the browser as uid 0 ({SANDBOX_UID_ENV} must be non-root)"
        );
        return None;
    }

    let ipt = iptables_bin();
    let ipt6 = ip6tables_bin();
    let uid_s = uid.to_string();
    let port = addr.port().to_string();
    // The guest is a single address family; `accept_bin` gets the one ACCEPT,
    // `other_bin` gets a blanket DROP (that family has no allowed destination).
    // Only one match arm runs, so moving `ipt`/`ipt6` into the tuple is fine.
    let (accept_bin, other_bin, dest) = match addr.ip() {
        IpAddr::V4(v4) => (ipt, ipt6, format!("{v4}/32")),
        IpAddr::V6(v6) => (ipt6, ipt, format!("{v6}/128")),
    };

    let mut conf = Confinement {
        uid,
        teardown: Vec::new(),
    };

    // 1) ACCEPT tcp to exactly the guest addr:port, for our uid only, at the
    //    very top of OUTPUT (so it precedes any pre-existing host ACCEPT).
    let accept_body = vec![
        "-d".into(),
        dest,
        "-p".into(),
        "tcp".into(),
        "--dport".into(),
        port,
        "-m".into(),
        "owner".into(),
        "--uid-owner".into(),
        uid_s.clone(),
        "-j".into(),
        "ACCEPT".into(),
    ];
    if !install_output_rule(&mut conf, &accept_bin, "1", &accept_body) {
        eprintln!(
            "[screenshot] skip: could not install guest-only ACCEPT rule (iptables unavailable?); capturing nothing"
        );
        return None; // conf drops → any partial rule removed
    }

    // 2) DROP everything else our uid emits, right below the ACCEPT.
    let drop_body = vec![
        "-m".into(),
        "owner".into(),
        "--uid-owner".into(),
        uid_s,
        "-j".into(),
        "DROP".into(),
    ];
    if !install_output_rule(&mut conf, &accept_bin, "2", &drop_body) {
        eprintln!("[screenshot] skip: could not install deny-by-default rule; capturing nothing");
        return None;
    }

    // 3) DROP the OTHER IP family entirely for our uid (the guest lives in one
    //    family; the other has no allowed destination at all). Strict: if this
    //    cannot be locked we fail closed rather than leave a v6/v4 escape hatch.
    if !install_output_rule(&mut conf, &other_bin, "1", &drop_body) {
        eprintln!(
            "[screenshot] skip: could not lock the other IP family (ip6tables unavailable?); capturing nothing"
        );
        return None;
    }

    Some(conf)
}

/// Insert one `OUTPUT` rule (`<bin> -I OUTPUT <pos> <body...>`) and record its
/// deletion spec on `conf` for teardown. Best-effort pre-cleans any stale copy
/// of the exact spec first (a crashed prior run). Returns `false` if the insert
/// does not succeed — the caller then fails closed.
#[cfg(unix)]
fn install_output_rule(conf: &mut Confinement, bin: &str, pos: &str, body: &[String]) -> bool {
    // Position-independent delete spec, reused for pre-clean AND teardown.
    let mut del = vec!["-D".to_string(), "OUTPUT".to_string()];
    del.extend_from_slice(body);
    for _ in 0..4 {
        if !run_ipt(bin, &del) {
            break;
        }
    }

    let mut ins = vec!["-I".to_string(), "OUTPUT".to_string(), pos.to_string()];
    ins.extend_from_slice(body);
    if !run_ipt(bin, &ins) {
        return false;
    }
    conf.teardown.push((bin.to_string(), del));
    true
}

/// Run `<bin> <args...>` silently; `true` iff it exited 0. A missing binary /
/// spawn error is `false` (⇒ fail closed at the call site).
#[cfg(unix)]
fn run_ipt(bin: &str, args: &[String]) -> bool {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True iff the current effective uid is root. Confinement (dedicated uid +
/// egress rules) is unavailable otherwise, so this gates fail-closed.
#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: geteuid is always safe and has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// The unprivileged uid the browser runs as (`ATO_SCREENSHOT_UID` or the fixed
/// default). Never 0 in practice — [`establish_confinement`] rejects 0.
#[cfg(unix)]
fn sandbox_uid() -> u32 {
    std::env::var(SANDBOX_UID_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_SANDBOX_UID)
}

#[cfg(unix)]
fn iptables_bin() -> String {
    env_bin(IPTABLES_BIN_ENV, "iptables")
}

#[cfg(unix)]
fn ip6tables_bin() -> String {
    env_bin(IP6TABLES_BIN_ENV, "ip6tables")
}

#[cfg(unix)]
fn env_bin(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Run Chrome as unprivileged `uid`, keep it alive over the local DevTools pipe,
/// and capture only after the client-render budget. The pipe is inherited as
/// Chrome's fixed fd 3 (commands) and fd 4 (responses); it does not widen the
/// browser's egress cage or expose a builder-host TCP listener.
#[cfg(unix)]
fn run_capture(bin: &str, out_path: &Path, url: &str, uid: u32) -> Result<(), String> {
    let dir = out_path.parent().unwrap_or_else(|| Path::new("."));
    // A throwaway, uid-owned profile: no shared/persistent browser state, no
    // access to any real profile.
    let profile = dir.join("profile");

    let (mut command_parent, command_child) =
        UnixStream::pair().map_err(|e| format!("create DevTools command pipe: {e}"))?;
    let (response_parent, response_child) =
        UnixStream::pair().map_err(|e| format!("create DevTools response pipe: {e}"))?;
    let command_child_fd = command_child.as_raw_fd();
    let response_child_fd = response_child.as_raw_fd();

    let mut cmd = Command::new(bin);
    cmd.arg("--headless=new")
        .arg("--remote-debugging-pipe")
        .arg("--window-size=1280,800")
        .arg("--run-all-compositor-stages-before-draw")
        // `--no-sandbox` is RETAINED deliberately (see module docs). Dropping it
        // needs Chromium's in-process sandbox to initialize as a NON-root uid,
        // which is not reliable across builder kernels (e.g. Ubuntu's
        // unprivileged-userns restriction) and would silently yield zero
        // thumbnails. Compensation: the browser is NOT root (runs as `uid`) and
        // is kernel-egress-locked to the single guest addr, so a renderer
        // exploit lands as an unprivileged, network-isolated uid.
        .arg("--no-sandbox")
        .arg("--disable-gpu") // no GPU on the builder
        .arg("--hide-scrollbars") // cleaner thumbnail
        .arg("--disable-extensions") // no extensions: attack surface + determinism
        .arg("--disable-dev-shm-usage") // /dev/shm may be tiny/unwritable for `uid`
        .arg("--disable-background-networking") // kill phone-home (would just hit DROP + waste budget)
        .arg("--disable-component-update") // same rationale
        .arg("--disable-sync") // no account sync attempts
        .arg("--no-first-run") // skip first-run setup/network
        .arg("--no-default-browser-check")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        // Keep the browser out of root's HOME (which `uid` cannot read anyway).
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: `pre_exec` is unsafe, and so are the raw uid/gid syscalls in its
    // closure (both covered by this one `unsafe` block). The closure calls only
    // async-signal-safe syscalls (setgroups/setgid/setuid), does no allocation
    // or locking, runs after fork / before exec, and checks each return value.
    // Order matters: drop supplementary groups and the gid BEFORE the uid (once
    // non-root you can no longer setgid).
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(command_child_fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::dup2(response_child_fd, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(uid as libc::gid_t) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid as libc::uid_t) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn {bin}: {e}"))?;
    drop(command_child);
    drop(response_child);

    let deadline = Instant::now() + CAPTURE_TIMEOUT;
    let result = (|| {
        let mut cdp = CdpPipe::new(&mut command_parent, response_parent, deadline);
        let target = cdp.request(
            1,
            "Target.createTarget",
            serde_json::json!({"url": url}),
            None,
        )?;
        let target_id = target
            .get("targetId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "DevTools createTarget omitted targetId".to_string())?;
        let attached = cdp.request(
            2,
            "Target.attachToTarget",
            serde_json::json!({"targetId": target_id, "flatten": true}),
            None,
        )?;
        let session_id = attached
            .get("sessionId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "DevTools attachToTarget omitted sessionId".to_string())?
            .to_string();
        cdp.request(
            3,
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({
                "width": 1280,
                "height": 800,
                "deviceScaleFactor": 1,
                "mobile": false
            }),
            Some(&session_id),
        )?;
        cdp.request(4, "Page.enable", serde_json::json!({}), Some(&session_id))?;
        // `Target.createTarget` may leave the app in a background target.
        // Background throttling can prevent requestAnimationFrame and
        // IntersectionObserver-driven landing pages from painting even though
        // their HTTP readiness check has passed.
        cdp.request(
            5,
            "Page.bringToFront",
            serde_json::json!({}),
            Some(&session_id),
        )?;
        std::thread::sleep(RENDER_SETTLE_BUDGET);
        let mut png = capture_png(&mut cdp, 6, &session_id)?;
        if crate::authoring_evidence::analyze_screenshot_png(&png).is_err() {
            std::thread::sleep(RENDER_RETRY_BUDGET);
            png = capture_png(&mut cdp, 7, &session_id)?;
        }
        std::fs::write(out_path, png).map_err(|e| format!("write screenshot: {e}"))
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(unix)]
fn capture_png(cdp: &mut CdpPipe<'_>, id: u64, session_id: &str) -> Result<Vec<u8>, String> {
    let captured = cdp.request(
        id,
        "Page.captureScreenshot",
        serde_json::json!({
            "format": "png",
            "fromSurface": true,
            "captureBeyondViewport": false
        }),
        Some(session_id),
    )?;
    let encoded = captured
        .get("data")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "DevTools captureScreenshot omitted PNG data".to_string())?;
    BASE64
        .decode(encoded)
        .map_err(|e| format!("decode DevTools PNG: {e}"))
}

#[cfg(unix)]
struct CdpPipe<'a> {
    writer: &'a mut UnixStream,
    reader: UnixStream,
    pending: Vec<u8>,
    deadline: Instant,
}

#[cfg(unix)]
impl<'a> CdpPipe<'a> {
    fn new(writer: &'a mut UnixStream, reader: UnixStream, deadline: Instant) -> Self {
        Self {
            writer,
            reader,
            pending: Vec::new(),
            deadline,
        }
    }

    fn request(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut message = serde_json::json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            message["sessionId"] = serde_json::Value::String(session_id.to_string());
        }
        let bytes = serde_json::to_vec(&message)
            .map_err(|e| format!("encode DevTools command {method}: {e}"))?;
        self.writer
            .write_all(&bytes)
            .and_then(|_| self.writer.write_all(&[0]))
            .and_then(|_| self.writer.flush())
            .map_err(|e| format!("write DevTools command {method}: {e}"))?;
        loop {
            let response = self.read_message()?;
            if response.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = response.get("error") {
                return Err(format!("DevTools command {method} failed: {error}"));
            }
            return Ok(response
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
    }

    fn read_message(&mut self) -> Result<serde_json::Value, String> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == 0) {
                let message: Vec<u8> = self.pending.drain(..end).collect();
                self.pending.drain(..1);
                return serde_json::from_slice(&message)
                    .map_err(|e| format!("decode DevTools response: {e}"));
            }
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| format!("DevTools capture timed out after {CAPTURE_TIMEOUT:?}"))?;
            self.reader
                .set_read_timeout(Some(remaining.min(Duration::from_secs(1))))
                .map_err(|e| format!("set DevTools read timeout: {e}"))?;
            let mut buf = [0u8; 8192];
            match self.reader.read(&mut buf) {
                Ok(0) => return Err("Chrome closed the DevTools response pipe".to_string()),
                Ok(read) => {
                    self.pending.extend_from_slice(&buf[..read]);
                    if self.pending.len() > MAX_CDP_MESSAGE_BYTES {
                        return Err("DevTools response exceeded size limit".to_string());
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(format!("read DevTools response: {error}")),
            }
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

    #[cfg(unix)]
    use std::sync::Mutex;

    // Rust runs tests in parallel and the process env is shared, so serialize
    // the env-mutating tests against each other. Poison-tolerant: a panic in one
    // env test must not wedge the others.
    #[cfg(unix)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── confinement fail-closed properties ──────────────────────────────────

    /// The core property: when the firewall tooling is unreachable, the cage
    /// cannot be built, so [`establish_confinement`] returns `None` — never a
    /// "capture anyway, unconfined". Forces the tooling nonexistent so the
    /// host's real iptables is never touched (side-effect free, no root needed:
    /// non-root short-circuits at `is_root`, root short-circuits at the missing
    /// binary).
    #[cfg(unix)]
    #[test]
    fn confinement_unavailable_yields_none() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env access serialized by ENV_LOCK; restored below.
        unsafe {
            std::env::set_var(IPTABLES_BIN_ENV, "/nonexistent/ato-screenshot-iptables");
            std::env::set_var(IP6TABLES_BIN_ENV, "/nonexistent/ato-screenshot-ip6tables");
        }
        let addr: SocketAddr = "172.16.0.2:8080".parse().unwrap();
        let conf = establish_confinement(addr);
        unsafe {
            std::env::remove_var(IPTABLES_BIN_ENV);
            std::env::remove_var(IP6TABLES_BIN_ENV);
        }
        assert!(
            conf.is_none(),
            "confinement must fail closed when the firewall tool is unavailable"
        );
    }

    /// End-to-end fail-closed: a *resolvable* browser (`/bin/echo` answers
    /// `--version` with exit 0) plus an unavailable cage MUST yield `None`, and
    /// `/bin/echo` must never be exec'd as an (unconfined) capture. This proves
    /// capture bails at the cage step, not after running an unconfined browser.
    #[cfg(unix)]
    #[test]
    fn browser_present_but_unconfined_capture_never_runs() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env access serialized by ENV_LOCK; restored below.
        unsafe {
            std::env::set_var(BROWSER_BIN_ENV, "/bin/echo");
            std::env::set_var(IPTABLES_BIN_ENV, "/nonexistent/ato-screenshot-iptables");
            std::env::set_var(IP6TABLES_BIN_ENV, "/nonexistent/ato-screenshot-ip6tables");
        }
        let addr: SocketAddr = "172.16.0.2:8080".parse().unwrap();
        let result = capture_best_effort(addr);
        unsafe {
            std::env::remove_var(BROWSER_BIN_ENV);
            std::env::remove_var(IPTABLES_BIN_ENV);
            std::env::remove_var(IP6TABLES_BIN_ENV);
        }
        assert!(
            result.is_none(),
            "must fail closed: browser present but confinement unavailable ⇒ no capture"
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_binary_yields_none_without_panicking() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env access serialized by ENV_LOCK; restored below.
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

    // ── output validation (portable) ────────────────────────────────────────

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

    #[cfg(unix)]
    #[test]
    fn client_render_budget_remains_below_the_capture_timeout() {
        assert!(RENDER_SETTLE_BUDGET + RENDER_RETRY_BUDGET < CAPTURE_TIMEOUT);
        assert_eq!(RENDER_SETTLE_BUDGET.as_millis(), 20_000);
        assert_eq!(RENDER_RETRY_BUDGET.as_millis(), 10_000);
        assert_eq!(CAPTURE_TIMEOUT.as_millis(), 60_000);
    }

    #[cfg(unix)]
    #[test]
    fn devtools_pipe_ignores_events_and_matches_the_command_id() {
        let (mut command_parent, mut command_child) = UnixStream::pair().unwrap();
        let (response_parent, mut response_child) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut request = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                command_child.read_exact(&mut byte).unwrap();
                if byte[0] == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request["id"], 7);
            response_child
                .write_all(b"{\"method\":\"Page.loadEventFired\"}\0")
                .unwrap();
            response_child
                .write_all(b"{\"id\":7,\"result\":{\"data\":\"png\"}}\0")
                .unwrap();
        });
        let mut pipe = CdpPipe::new(
            &mut command_parent,
            response_parent,
            Instant::now() + Duration::from_secs(1),
        );
        let result = pipe
            .request(7, "Page.captureScreenshot", serde_json::json!({}), None)
            .unwrap();
        assert_eq!(result["data"], "png");
        server.join().unwrap();
    }
}
