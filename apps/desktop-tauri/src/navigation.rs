//! Navigation classification and `ato://` intent parsing for the Tauri shell.
//!
//! These are the pure, host-independent rules that decide whether a top-level
//! navigation is allowed inside a WebView, handed to the OS browser, or
//! intercepted as a native intent. Keeping them free of Tauri window plumbing
//! makes them unit-testable and reviewable on their own.

use tauri::Url;

pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
pub(crate) const HOME_WINDOW_LABEL: &str = "home";
pub(crate) const HOME_ORIGIN: &str = "https://app.ato.run";

/// What the shell should do with a top-level navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationAction {
    /// Continue the navigation inside the current WebView.
    Allow,
    /// Cancel the navigation and hand the `ato://` URI to Rust for parsing.
    DispatchIntent,
    /// Cancel the navigation and open the URL in the OS browser.
    OpenExternal,
    /// Refuse the navigation entirely.
    Deny,
}

/// The trust role of the window that is navigating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationRole {
    /// Bundled launcher assets; the only window with native capability.
    Main,
    /// The remote Home (`https://app.ato.run`); no native capability.
    Home,
    /// A guest surface window pinned to one exact loopback origin.
    App { origin: String },
}

/// Classify a top-level navigation against the window's trust role.
///
/// Fail-closed: userinfo-bearing URLs are always denied, and any scheme that is
/// not explicitly listed for the role is refused. `ato://` is always
/// intercepted for Rust-side parsing rather than rendered.
pub fn classify(url: &Url, role: &NavigationRole) -> NavigationAction {
    if !url.username().is_empty() || url.password().is_some() {
        return NavigationAction::Deny;
    }
    if url.scheme() == "ato" {
        return NavigationAction::DispatchIntent;
    }
    match role {
        NavigationRole::Main => {
            if is_local_launcher_url(url) {
                NavigationAction::Allow
            } else if is_http(url) {
                NavigationAction::OpenExternal
            } else {
                NavigationAction::Deny
            }
        }
        NavigationRole::Home => {
            if is_trusted_home_url(url) || url.scheme() == "about" {
                NavigationAction::Allow
            } else if is_http(url) {
                NavigationAction::OpenExternal
            } else {
                NavigationAction::Deny
            }
        }
        NavigationRole::App { origin } => {
            if url_origin(url) == *origin {
                NavigationAction::Allow
            } else if is_http(url) {
                NavigationAction::OpenExternal
            } else {
                NavigationAction::Deny
            }
        }
    }
}

/// Whether a URL is one of the launcher's own local asset origins.
pub fn is_local_launcher_url(url: &Url) -> bool {
    url.scheme() == "tauri"
        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
}

/// Whether a URL is the exact, HTTPS-only Home origin.
pub fn is_trusted_home_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("app.ato.run")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

/// Whether a URL is a loopback HTTP(S) origin acceptable as a guest surface.
/// Hostnames that merely end in a loopback label (e.g. `127.0.0.1.evil.example`)
/// are refused because host matching is exact.
pub fn is_loopback_surface_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        && url.username().is_empty()
        && url.password().is_none()
}

/// The exact origin (`scheme://host:port`) used to pin a guest window.
pub fn url_origin(url: &Url) -> String {
    format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().unwrap_or_default(),
        url.port_or_known_default().unwrap_or_default()
    )
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// A native intent carried by an intercepted `ato://` URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Bring the remote Home window to the front.
    OpenHome,
}

/// Parse an `ato://` URI into a recognized intent. Returns `None` for any
/// other scheme, namespace, or action — the caller must then reject it.
pub fn parse_intent(url: &Url) -> Option<Intent> {
    if url.scheme() != "ato" {
        return None;
    }
    match (url.host_str(), url.path()) {
        (Some("desktop"), "/home") => Some(Intent::OpenHome),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        value.parse().unwrap()
    }

    #[test]
    fn main_allows_only_local_assets_and_bounces_the_rest() {
        let role = NavigationRole::Main;
        assert_eq!(
            classify(&url("tauri://localhost/index.html"), &role),
            NavigationAction::Allow
        );
        assert_eq!(
            classify(&url("http://tauri.localhost/index.html"), &role),
            NavigationAction::Allow
        );
        assert_eq!(
            classify(&url("https://example.com/"), &role),
            NavigationAction::OpenExternal
        );
        assert_eq!(
            classify(&url("file:///etc/passwd"), &role),
            NavigationAction::Deny
        );
    }

    #[test]
    fn home_is_pinned_to_the_exact_https_origin() {
        let role = NavigationRole::Home;
        assert_eq!(
            classify(&url("https://app.ato.run/store"), &role),
            NavigationAction::Allow
        );
        assert_eq!(
            classify(&url("http://app.ato.run/store"), &role),
            NavigationAction::OpenExternal
        );
        assert_eq!(
            classify(&url("https://evil.app.ato.run/"), &role),
            NavigationAction::OpenExternal
        );
        assert_eq!(
            classify(&url("https://app.ato.run.evil.example/"), &role),
            NavigationAction::OpenExternal
        );
    }

    #[test]
    fn app_window_is_pinned_to_its_exact_loopback_origin() {
        let role = NavigationRole::App {
            origin: "http://127.0.0.1:8000".to_owned(),
        };
        assert_eq!(
            classify(&url("http://127.0.0.1:8000/"), &role),
            NavigationAction::Allow
        );
        assert_eq!(
            classify(&url("http://127.0.0.1:8000/app/page"), &role),
            NavigationAction::Allow
        );
        assert_eq!(
            classify(&url("http://127.0.0.1:8001/"), &role),
            NavigationAction::OpenExternal
        );
        assert_eq!(
            classify(&url("https://example.com/"), &role),
            NavigationAction::OpenExternal
        );
    }

    #[test]
    fn loopback_surface_matching_is_exact() {
        assert!(is_loopback_surface_url(&url("http://127.0.0.1:4317/")));
        assert!(is_loopback_surface_url(&url("http://localhost:4317/")));
        assert!(!is_loopback_surface_url(&url("https://app.ato.run/")));
        assert!(!is_loopback_surface_url(&url(
            "http://127.0.0.1.evil.example/"
        )));
    }

    #[test]
    fn userinfo_urls_are_always_denied() {
        for role in [
            NavigationRole::Main,
            NavigationRole::Home,
            NavigationRole::App {
                origin: "http://127.0.0.1:8000".to_owned(),
            },
        ] {
            assert_eq!(
                classify(&url("http://user:pass@127.0.0.1:8000/"), &role),
                NavigationAction::Deny
            );
        }
    }

    #[test]
    fn ato_intents_are_intercepted_and_unknown_ones_rejected() {
        assert_eq!(
            parse_intent(&url("ato://desktop/home")),
            Some(Intent::OpenHome)
        );
        assert_eq!(parse_intent(&url("ato://desktop/install?source=x")), None);
        assert_eq!(parse_intent(&url("ato://runner/start")), None);
        assert_eq!(parse_intent(&url("https://app.ato.run/")), None);
    }

    #[test]
    fn ato_scheme_is_always_dispatched_for_parsing() {
        assert_eq!(
            classify(&url("ato://desktop/home"), &NavigationRole::Main),
            NavigationAction::DispatchIntent
        );
        assert_eq!(
            classify(&url("ato://runner/start"), &NavigationRole::Home),
            NavigationAction::DispatchIntent
        );
    }
}
