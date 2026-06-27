//! Desktop → embedded-PWA Home handoff.
//!
//! When the Desktop embeds the PWA as its Home surface, it hands the PWA a
//! loopback Runtime Control endpoint plus a **Desktop-scoped** bearer token via
//! the PWA's existing `#endpoint=&token=` fragment convention (the same shape
//! `ato console open` already produces — see `cli/src/cli/dispatch/console.rs`).
//! The embedded PWA then auto-connects its `RuntimeApiClient` and can show
//! "Running locally on this Desktop" state on its app cards.
//!
//! ## Security invariants
//!
//! * The control token is generated once per Desktop process, cached for its
//!   lifetime, and is **never logged**.
//! * Only a *trusted* PWA origin (prod `app.ato.run` / `stg-app.ato.run`, or a
//!   loopback dev origin in debug builds) ever receives the endpoint + token.
//!   An untrusted origin gets a plain URL with no credentials
//!   ([`build_pwa_home_url`]).
//! * The Runtime Control server is ensured to run with this token so its
//!   privileged endpoints (post-hardening) accept the Desktop's own calls
//!   while rejecting tokenless callers.
//!
//! The handoff is **opt-in** via `ATO_PWA_HOME_ENABLED`; with the flag unset the
//! Desktop's existing Start/Settings/Store/Dock behavior is unchanged.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use capsule::common::paths::ato_path_or_workspace_tmp;
use url::Url;

/// Prod PWA Home origin used when no override is configured.
const DEFAULT_PWA_HOME_URL: &str = "https://app.ato.run";
/// Env override for the PWA Home base URL (staging / local dev).
const PWA_HOME_URL_ENV: &str = "ATO_PWA_HOME_URL";
/// Env opt-in flag: when truthy, Desktop Home embeds the PWA.
const PWA_HOME_ENABLED_ENV: &str = "ATO_PWA_HOME_ENABLED";

/// Per-process Desktop-scoped Runtime Control bearer token.
static RUNTIME_CONTROL_TOKEN: OnceLock<String> = OnceLock::new();

/// The Desktop-scoped Runtime Control bearer token for this process.
///
/// Resolved once (then cached): if a local registry is already running it has
/// persisted its token to `<local-registry>/.console-token` (the same file
/// `ato console` reads and `ato registry serve --auth-token` writes), so we
/// reuse that to stay in sync across Desktop restarts and with an
/// already-running server. Otherwise a fresh 256-bit token is generated and
/// will be persisted when we start the server. The token MUST NOT be logged.
pub(crate) fn runtime_control_token() -> &'static str {
    RUNTIME_CONTROL_TOKEN.get_or_init(load_or_generate_token)
}

/// Directory the local registry uses for state (and where it persists its
/// console token). Matches the `--data-dir` we pass when starting it.
fn local_registry_dir() -> PathBuf {
    ato_path_or_workspace_tmp("local-registry")
}

/// Path to the persisted Runtime Control / console bearer token.
fn console_token_path() -> PathBuf {
    local_registry_dir().join(".console-token")
}

/// Reuse a persisted token when present, else generate a fresh one.
fn load_or_generate_token() -> String {
    if let Ok(raw) = std::fs::read_to_string(console_token_path()) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    generate_token()
}

/// Generate a 256-bit token, hex-encoded (64 lowercase hex chars).
fn generate_token() -> String {
    let mut buf = [0u8; 32];
    if getrandom::getrandom(&mut buf).is_err() {
        // Extremely unlikely (CSPRNG unavailable). Degrade to a time + pid
        // derived value so the Desktop still starts; the token remains a
        // loopback-only, opt-in credential. Not used on supported platforms.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = u128::from(std::process::id());
        let mut mix = nanos ^ (pid << 64) ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for chunk in buf.chunks_mut(16) {
            mix = mix
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let bytes = mix.to_le_bytes();
            for (slot, byte) in chunk.iter_mut().zip(bytes.iter()) {
                *slot = *byte;
            }
        }
    }
    let mut out = String::with_capacity(64);
    for byte in buf {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'a' + (nibble - 10)) as char,
    }
}

/// Whether the embedded-PWA Home surface is enabled (opt-in).
pub(crate) fn pwa_home_enabled() -> bool {
    matches!(
        std::env::var(PWA_HOME_ENABLED_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

/// The configured PWA Home base URL (env override or prod default).
pub(crate) fn pwa_home_base_url() -> String {
    std::env::var(PWA_HOME_URL_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PWA_HOME_URL.to_string())
}

/// Whether the desktop build runs in dev mode (debug build). Loopback PWA
/// origins are only trusted with the runtime handoff in dev mode.
pub(crate) fn is_dev_mode() -> bool {
    cfg!(debug_assertions)
}

/// Origin-trust gate: only these origins ever receive the Desktop runtime
/// endpoint + token. Everything else gets a plain URL with no credentials.
///
/// * `https://app.ato.run` and `https://stg-app.ato.run` — always trusted.
/// * Loopback dev origins (`localhost` / `127.0.0.1`, any port) — trusted only
///   in dev (debug) builds.
pub(crate) fn is_trusted_pwa_origin(url: &Url, dev_mode: bool) -> bool {
    if url.scheme() == "https" {
        if let Some("app.ato.run" | "stg-app.ato.run") = url.host_str() {
            return true;
        }
    }
    if dev_mode {
        if let Some(host) = url.host_str() {
            let is_loopback = host == "localhost" || host == "127.0.0.1";
            if is_loopback && matches!(url.scheme(), "http" | "https") {
                return true;
            }
        }
    }
    false
}

/// Build the embedded PWA Home URL.
///
/// For a **trusted** origin, append the `#route=/&endpoint=<enc>&token=<enc>`
/// fragment the PWA consumes to auto-connect to the local Runtime Control API
/// (the PWA strips the sensitive parts from its address bar on load via
/// `clearSensitiveFragment()`). For an **untrusted** origin, return the base URL
/// with `#route=/` only — no credentials are ever attached.
pub(crate) fn build_pwa_home_url(
    base_url: &str,
    runtime_port: u16,
    token: &str,
    dev_mode: bool,
) -> Result<Url> {
    let mut url =
        Url::parse(base_url).with_context(|| format!("parse PWA home url '{base_url}'"))?;
    if is_trusted_pwa_origin(&url, dev_mode) {
        let endpoint = format!("http://127.0.0.1:{runtime_port}");
        let fragment = format!(
            "route=/&endpoint={}&token={}",
            fragment_encode(&endpoint),
            fragment_encode(token),
        );
        url.set_fragment(Some(&fragment));
    } else {
        url.set_fragment(Some("route=/"));
    }
    Ok(url)
}

/// Minimal percent-encoding for URL fragment components. Mirrors the encoding
/// used by `ato console`'s `build_console_url` so the PWA's `URLSearchParams`
/// fragment parser decodes the values identically.
fn fragment_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => {
                out.push('%');
                out.push(hex_digit_upper(other >> 4));
                out.push(hex_digit_upper(other & 0x0f));
            }
        }
    }
    out
}

fn hex_digit_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Whether something is already listening on the loopback Runtime Control port.
fn is_runtime_serve_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

/// Best-effort: ensure `ato registry serve` is running on `port` with `token`.
///
/// If something is already listening on the port we leave it alone (it may be a
/// user-started registry; reads still work, and a token mismatch only affects
/// privileged writes). Otherwise we spawn a detached `ato registry serve`
/// bound to loopback with the Desktop-scoped token. The token is passed as a
/// CLI arg and is never logged here.
pub(crate) fn ensure_runtime_serve(port: u16, token: &str) -> Result<()> {
    if is_runtime_serve_listening(port) {
        tracing::debug!(port, "runtime control server already listening; reusing");
        return Ok(());
    }
    let ato = crate::orchestrator::resolve_ato_binary()
        .context("resolve ato binary for runtime serve")?;
    tracing::info!(port, "starting ato registry serve for embedded PWA home");
    let data_dir = local_registry_dir();
    let mut command = std::process::Command::new(&ato);
    command
        .arg("registry")
        .arg("serve")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--auth-token")
        .arg(token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command
        .spawn()
        .with_context(|| format!("spawn `ato registry serve` on port {port}"))?;
    Ok(())
}

/// Open the Desktop Home surface.
///
/// When the embedded-PWA Home is enabled, open the PWA (ensuring the runtime
/// server is up and handing off the endpoint + token), falling back to the
/// native Start window on any synchronous failure (bad URL, etc.). When the
/// flag is unset, behave exactly as before and open the native Start window.
pub(crate) fn open_home(cx: &mut gpui::App) -> Result<()> {
    let config = crate::config::load_config();
    let port = config.registry.local_registry_port;

    // Ensure the Runtime Control server is up with the Desktop-scoped token.
    // Both Home surfaces depend on it: the native Start page already streams
    // `runtime_base_url` reads, and its launch/stop handlers — like the embedded
    // PWA — now authenticate with this token (the control plane fails closed
    // without one). Best-effort; failure is non-fatal and only degrades the
    // local-session features.
    if let Err(err) = ensure_runtime_serve(port, runtime_control_token()) {
        tracing::warn!(error = %err, "could not ensure runtime serve for Home");
    }

    if !pwa_home_enabled() {
        return crate::window::start_window::open_start_window(cx);
    }
    match open_pwa_home_window(cx, port) {
        Ok(()) => Ok(()),
        Err(err) => {
            tracing::error!(
                error = %err,
                "embedded PWA home failed to open; falling back to native Start window"
            );
            crate::window::start_window::open_start_window(cx)
        }
    }
}

/// Open the embedded PWA Home as an external-URL WebView window with the
/// runtime handoff applied. Reuses the existing `open_app_window` path so the
/// Desktop chrome, sidebar, and reload behavior are identical to any other
/// external-URL surface.
fn open_pwa_home_window(cx: &mut gpui::App, port: u16) -> Result<()> {
    let base = pwa_home_base_url();
    let url = build_pwa_home_url(&base, port, runtime_control_token(), is_dev_mode())?;
    crate::window::orchestrator::open_app_window(cx, crate::state::GuestRoute::ExternalUrl(url))
        .map(|_handle| ())
        .context("open embedded PWA home window")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_control_token_is_stable() {
        // The process token is resolved once and stable thereafter. (We do not
        // assert its format here: it may be reused from an existing
        // `.console-token` on the host, which need not be our hex shape.)
        assert_eq!(runtime_control_token(), runtime_control_token());
    }

    #[test]
    fn generated_token_is_hex_256_bit() {
        let t = generate_token();
        assert_eq!(t.len(), 64, "256-bit token = 64 hex chars");
        assert!(t.bytes().all(|c| c.is_ascii_hexdigit()), "token must be hex");
    }

    #[test]
    fn generate_token_is_unpredictable() {
        // Two freshly generated tokens must differ (CSPRNG).
        assert_ne!(generate_token(), generate_token());
    }

    #[test]
    fn prod_origin_is_trusted() {
        let url = Url::parse("https://app.ato.run").unwrap();
        assert!(is_trusted_pwa_origin(&url, false));
        assert!(is_trusted_pwa_origin(&url, true));
    }

    #[test]
    fn staging_origin_is_trusted() {
        let url = Url::parse("https://stg-app.ato.run/").unwrap();
        assert!(is_trusted_pwa_origin(&url, false));
    }

    #[test]
    fn unknown_origin_is_never_trusted() {
        let evil = Url::parse("https://evil.example.com").unwrap();
        assert!(!is_trusted_pwa_origin(&evil, false));
        assert!(!is_trusted_pwa_origin(&evil, true));
        // app.ato.run over plain http is NOT the trusted https origin.
        let http = Url::parse("http://app.ato.run").unwrap();
        assert!(!is_trusted_pwa_origin(&http, false));
    }

    #[test]
    fn loopback_origin_trusted_only_in_dev() {
        let local = Url::parse("http://localhost:5173").unwrap();
        let loopback = Url::parse("http://127.0.0.1:5173").unwrap();
        assert!(is_trusted_pwa_origin(&local, true));
        assert!(is_trusted_pwa_origin(&loopback, true));
        assert!(!is_trusted_pwa_origin(&local, false));
        assert!(!is_trusted_pwa_origin(&loopback, false));
    }

    #[test]
    fn handoff_url_includes_endpoint_and_token_for_trusted_origin() {
        let url = build_pwa_home_url("https://app.ato.run", 8080, "abc123token", false).unwrap();
        let fragment = url.fragment().expect("fragment present");
        assert!(fragment.contains("endpoint=http%3A%2F%2F127.0.0.1%3A8080"));
        assert!(fragment.contains("token=abc123token"));
        assert!(fragment.contains("route=/"));
    }

    #[test]
    fn handoff_url_omits_credentials_for_untrusted_origin() {
        let url =
            build_pwa_home_url("https://evil.example.com", 8080, "secret-token", false).unwrap();
        let fragment = url.fragment().unwrap_or("");
        assert!(
            !fragment.contains("token="),
            "untrusted origin must not receive a token"
        );
        assert!(
            !fragment.contains("endpoint="),
            "untrusted origin must not receive an endpoint"
        );
        assert!(!fragment.contains("secret-token"));
        assert_eq!(fragment, "route=/");
    }

    #[test]
    fn loopback_handoff_only_in_dev() {
        // In production builds (dev_mode=false), a localhost PWA URL must not
        // receive credentials even though it is the configured home.
        let prod = build_pwa_home_url("http://localhost:5173", 8080, "tok", false).unwrap();
        assert!(!prod.fragment().unwrap_or("").contains("token="));
        // In dev it does.
        let dev = build_pwa_home_url("http://localhost:5173", 8080, "tok", true).unwrap();
        assert!(dev.fragment().unwrap_or("").contains("token=tok"));
    }

    #[test]
    fn fragment_encode_matches_console_contract() {
        assert_eq!(
            fragment_encode("http://127.0.0.1:8080"),
            "http%3A%2F%2F127.0.0.1%3A8080"
        );
        // Hex tokens pass through unchanged.
        assert_eq!(fragment_encode("deadbeef00"), "deadbeef00");
    }
}
