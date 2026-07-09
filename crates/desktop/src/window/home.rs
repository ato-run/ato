//! Home surface — the dedicated `ato-pwa` Home window (`app.ato.run`).
//!
//! `StartupSurface::Home` routes here; the PWA opens in the dedicated
//! [`crate::window::ato_home_shell`] window (single full-window WebView,
//! AtoDesktop UA + `window.__ATO_DESKTOP__` marker, `ato://`/`capsule://`
//! interception). ato-start is fully retired as a landing surface: when
//! the host is unreachable the WebView shows its own offline error page
//! and the user can retry from the Shell Icon Bar.

use anyhow::Result;
use gpui::App;

/// Default PWA origin used when `app_base_url` is unparseable.
const DEFAULT_APP_BASE_URL: &str = "https://app.ato.run";

/// Resolve the configured PWA Home URL. Pure so the URL-parse + fallback
/// policy is unit-testable without GPUI. Non-http(s) or unparseable
/// values fall back to the production PWA origin.
pub fn home_url(config: &crate::config::DesktopConfig) -> url::Url {
    let raw = config.desktop.app_base_url.trim();
    match url::Url::parse(raw) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        other => {
            tracing::warn!(
                result = ?other.map(|url| url.scheme().to_string()),
                url = %raw,
                "home: app_base_url unusable; falling back to the default PWA origin"
            );
            url::Url::parse(DEFAULT_APP_BASE_URL).expect("default PWA origin parses")
        }
    }
}

/// Raise the Ato Home surface — the fixed leading icon of the Shell
/// Icon Bar routes here. If the Home window is already open it is
/// focused instead of spawning a duplicate; otherwise a fresh Home
/// window opens via [`open_home_window`].
pub fn show_ato_home(cx: &mut App) -> Result<()> {
    use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};

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

/// Open the Home surface — always the dedicated PWA window. When the
/// host is offline the WebView shows its own error page; no native
/// fallback surface exists anymore.
pub fn open_home_window(cx: &mut App) -> Result<()> {
    let url = home_url(&crate::config::load_config());
    crate::window::ato_home_shell::open_ato_home_window(cx, url).map(|_| ())
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
    fn home_url_uses_configured_https_origin() {
        let config = config_with("https://stg-app.ato.run");
        assert_eq!(home_url(&config).as_str(), "https://stg-app.ato.run/");
    }

    #[test]
    fn home_url_allows_localhost_dev_http() {
        let config = config_with("http://localhost:5173");
        assert_eq!(home_url(&config).as_str(), "http://localhost:5173/");
    }

    #[test]
    fn home_url_falls_back_to_default_for_non_http_scheme() {
        let config = config_with("file:///tmp/home");
        assert_eq!(home_url(&config).as_str(), "https://app.ato.run/");
    }

    #[test]
    fn home_url_falls_back_to_default_when_unparseable() {
        let config = config_with("not a url");
        assert_eq!(home_url(&config).as_str(), "https://app.ato.run/");
    }
}
