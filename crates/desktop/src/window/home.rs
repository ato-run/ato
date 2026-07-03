//! Home surface — embeds the `ato-pwa` Home (`app.ato.run`) in a WebView.
//!
//! `StartupSurface::Home` routes here. The PWA is opened as a
//! [`GuestRoute::ExternalUrl`] app window, which flows through `WebViewManager`
//! and therefore **reuses** the existing System-route machinery rather than
//! re-implementing it:
//!   - the persistent shared cookie context (sign-in survives close/reopen),
//!   - the `AtoDesktop/<version>` User-Agent + `window.__ATO_DESKTOP__` marker
//!     (so the PWA can feature-gate Desktop-specific UX),
//!   - `ato://` navigation interception (the intent bridge — hardened in PR 2),
//!   - desktop→web auth-cookie injection (see
//!     [`crate::webview`] `should_install_ato_auth_cookies`).
//!
//! Offline safety: before opening the remote PWA we probe the host. When it is
//! unreachable we fall back to the native, bundled `ato-start` landing
//! ([`StartupSurface::Start`](crate::config::StartupSurface::Start)) so Home is
//! never a blank screen.

use std::time::Duration;

use anyhow::Result;
use gpui::App;

/// Connection-probe budget. Kept short so an offline launch falls back to the
/// native landing quickly rather than hanging on a dead network.
const HOME_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// What the Home surface should render, resolved from config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeTarget {
    /// Remote `ato-pwa` Home at this URL.
    Pwa(url::Url),
    /// Native bundled `ato-start` landing (offline fallback / explicit opt-out).
    Native,
}

/// Resolve the configured Home target. Pure so the URL-parse + fallback policy
/// is unit-testable without GPUI.
pub fn home_target(config: &crate::config::DesktopConfig) -> HomeTarget {
    let raw = config.desktop.app_base_url.trim();
    match url::Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => HomeTarget::Pwa(url),
        Ok(other) => {
            tracing::warn!(
                scheme = other.scheme(),
                "home: app_base_url is not http(s); using native landing"
            );
            HomeTarget::Native
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                url = %raw,
                "home: app_base_url unparseable; using native landing"
            );
            HomeTarget::Native
        }
    }
}

/// Blocking reachability probe. Returns `true` when the host responds at all
/// (any HTTP status counts — the server is up); `false` only on transport
/// failures (DNS / connect / TLS), i.e. genuinely offline. Run off the UI
/// thread.
pub fn probe_home_reachable(url: &str, timeout: Duration) -> bool {
    match ureq::head(url)
        .timeout(timeout)
        .set("User-Agent", "ato-desktop")
        .call()
    {
        // Server answered (even 4xx/5xx) → reachable.
        Ok(_) | Err(ureq::Error::Status(_, _)) => true,
        // DNS / connect / TLS transport failure → treat as offline.
        Err(ureq::Error::Transport(_)) => false,
    }
}

/// Raise the Ato Home surface — the fixed leading icon of the Shell
/// Icon Bar routes here. If a Home window (remote PWA or the native
/// `ato-start` fallback) is already open it is focused instead of
/// spawning a duplicate; otherwise a fresh Home window opens via
/// [`open_home_window`].
pub fn show_ato_home(cx: &mut App) -> Result<()> {
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

    // Prefer the dedicated PWA Home window; the native Start landing is
    // only a fallback surface, so don't let it shadow a fresh PWA open.
    let existing = cx
        .global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .find(|entry| matches!(entry.kind, ContentWindowKind::Home));
    if let Some(entry) = existing {
        let window_id = entry.handle.window_id().as_u64();
        // Probe liveness cheaply before treating the entry as raisable.
        if entry.handle.update(cx, |_, _, _| ()).is_ok() {
            cx.global_mut::<OpenContentWindows>().focus(window_id);
            crate::window::raise_content_window(cx, entry.handle);
            tracing::info!(window_id, "show_ato_home: raised existing Home window");
            return Ok(());
        }
    }
    tracing::info!("show_ato_home: no live Home window — opening a new one");
    open_home_window(cx)
}

/// Open the Home surface.
///
/// For the PWA target this probes reachability off the UI thread, then opens
/// either the remote PWA window or the native landing on the foreground
/// executor. Returns immediately; the window appears once the probe resolves
/// (sub-100ms when online, ≤ [`HOME_PROBE_TIMEOUT`] when offline). The probe is
/// deferred (rather than blocking) so startup is never stalled by the network —
/// the same off-thread + foreground-executor pattern used by
/// [`crate::window::start_window`] for its OCI-session refresh.
pub fn open_home_window(cx: &mut App) -> Result<()> {
    let config = crate::config::load_config();
    match home_target(&config) {
        HomeTarget::Native => crate::window::start_window::open_start_window(cx),
        HomeTarget::Pwa(url) => {
            let async_app = cx.to_async();
            async_app
                .foreground_executor()
                .spawn({
                    let be = async_app.background_executor().clone();
                    let aa = async_app.clone();
                    let probe_url = url.to_string();
                    async move {
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = tx.send(probe_home_reachable(&probe_url, HOME_PROBE_TIMEOUT));
                        });
                        let reachable = loop {
                            be.timer(Duration::from_millis(50)).await;
                            match rx.try_recv() {
                                Ok(value) => break value,
                                Err(std::sync::mpsc::TryRecvError::Empty) => continue,
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => break false,
                            }
                        };
                        let _ = aa.update(|cx| {
                            let result = if reachable {
                                crate::window::ato_home_shell::open_ato_home_window(
                                    cx,
                                    url.clone(),
                                )
                                .map(|_| ())
                            } else {
                                tracing::info!(
                                    "home: PWA host unreachable; falling back to native landing"
                                );
                                crate::window::start_window::open_start_window(cx)
                            };
                            if let Err(err) = result {
                                tracing::error!(error = %err, "home: failed to open Home surface");
                            }
                        });
                    }
                })
                .detach();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(app_base_url: &str) -> crate::config::DesktopConfig {
        let mut config = crate::config::DesktopConfig::default();
        config.desktop.app_base_url = app_base_url.to_string();
        config
    }

    #[test]
    fn home_target_uses_pwa_for_https() {
        let config = config_with("https://app.ato.run");
        assert_eq!(
            home_target(&config),
            HomeTarget::Pwa(url::Url::parse("https://app.ato.run").unwrap())
        );
    }

    #[test]
    fn home_target_allows_localhost_dev_http() {
        let config = config_with("http://localhost:5173");
        assert!(matches!(home_target(&config), HomeTarget::Pwa(_)));
    }

    #[test]
    fn home_target_trims_whitespace() {
        let config = config_with("  https://stg-app.ato.run  ");
        assert_eq!(
            home_target(&config),
            HomeTarget::Pwa(url::Url::parse("https://stg-app.ato.run").unwrap())
        );
    }

    #[test]
    fn home_target_falls_back_to_native_for_non_http_scheme() {
        let config = config_with("file:///tmp/home");
        assert_eq!(home_target(&config), HomeTarget::Native);
    }

    #[test]
    fn home_target_falls_back_to_native_when_unparseable() {
        let config = config_with("not a url");
        assert_eq!(home_target(&config), HomeTarget::Native);
    }
}
