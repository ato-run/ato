//! WebAppView — the dedicated single-WebView surface for web-served
//! apps (cloud/remote session URLs, `ato://open` targets, any
//! ExternalUrl AppWindow).
//!
//! NOT the web-viewer: no tab strip, no URL bar, no back/forward/reload
//! in any build. One Wry WebView fills the window; the window IS the
//! app. Non-web navigations (`capsule://…`, `ato://…`) are cancelled and
//! re-dispatched as [`NavigateToUrl`] so launches always open their own
//! independent window (and Shell Icon Bar tab) instead of navigating
//! this one away.
//!
//! The Ato Home shell reuses this view; its window-level concerns
//! (singleton slot, `ContentWindowKind::Home` registration) stay in
//! [`super::ato_home_shell`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, IntoElement, Pixels, Render, Size, div, rgb};
use serde::Deserialize;
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::app::NavigateToUrl;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

type InterceptedNavQueue = Arc<Mutex<Vec<String>>>;
type IpcLaunchQueue = Arc<Mutex<Vec<IpcLaunchRequest>>>;

/// One `window.__ATO_DESKTOP__.launch()` call queued by the wry IPC
/// handler (which must never touch GPUI state) for the drain loop.
#[derive(Debug, PartialEq)]
struct IpcLaunchRequest {
    /// The page-side request id; the ack resolves the matching pending
    /// promise via `window.__ATO_DESKTOP_PENDING__.resolve(id, …)`.
    request_id: u64,
    /// Validated handoff, or the rejection reason for the `accepted:false`
    /// ack. Only the launch_id + display ref ride this channel — payloads
    /// carrying URLs or token-shaped ids are rejected at parse time.
    outcome: Result<LaunchHandoff, String>,
}

#[derive(Debug, PartialEq)]
struct LaunchHandoff {
    launch_id: String,
    capsule_ref: String,
}

pub struct WebAppView {
    webview: Option<WebView>,
    window_size: Size<Pixels>,
    pub paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(WebAppView, paste);

impl WebViewPasteShell for WebAppView {
    fn active_paste_target(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }
}

impl Render for WebAppView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl WebAppView {
    /// Build the dedicated app WebView filling `window`, pointed at `url`.
    /// `context_label` names the surface in crash reports.
    pub fn new(
        context_label: &str,
        url: Url,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let nav_queue: InterceptedNavQueue = Arc::new(Mutex::new(Vec::new()));
        let queue_for_nav = nav_queue.clone();
        let ipc_queue: IpcLaunchQueue = Arc::new(Mutex::new(Vec::new()));
        let queue_for_ipc = ipc_queue.clone();
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        let builder = WebViewBuilder::new()
            .with_url(url.as_str())
            .with_bounds(webview_rect)
            // Desktop marker, layer 1: UA suffix so ato-served pages can
            // render Desktop-specific UX without JS detection.
            .with_user_agent(format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/{}",
                env!("CARGO_PKG_VERSION")
            ))
            // Desktop marker, layer 2: JS global injected before page
            // scripts so client code can feature-gate on it — plus the
            // launch-handoff bridge (`__ATO_DESKTOP__.launch`).
            .with_initialization_script(&desktop_init_script())
            // Launch handoff IPC: the wry callback runs off the GPUI
            // world — parse + validate only, queue for the drain loop
            // (same discipline as the intercepted-nav queue above).
            .with_ipc_handler(move |request| {
                if let Some(parsed) = parse_ipc_launch_message(request.body()) {
                    if let Ok(mut queue) = queue_for_ipc.lock() {
                        queue.push(parsed);
                    }
                }
            })
            // App-launch intents leave this window: cancel the in-page
            // navigation and queue it for the NavigateToUrl dispatch loop.
            .with_navigation_handler(move |target: String| -> bool {
                if is_web_navigation(&target) {
                    true
                } else {
                    if let Ok(mut q) = queue_for_nav.lock() {
                        q.push(target);
                    }
                    false
                }
            });
        let webview = crate::window::build_child_webview(context_label, builder, window);
        spawn_intercepted_nav_drain(cx, nav_queue, window.window_handle());
        spawn_ipc_launch_drain(cx, ipc_queue);
        Self {
            webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        }
    }

    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
            let _ = webview.set_bounds(Rect {
                position: LogicalPosition::new(0i32, 0i32).into(),
                size: LogicalSize::new(
                    f32::from(current.width) as u32,
                    f32::from(current.height) as u32,
                )
                .into(),
            });
        }
        self.window_size = current;
    }
}

/// Schemes the app WebView navigates itself; everything else is an
/// app-launch intent that must leave the window.
pub fn is_web_navigation(target: &str) -> bool {
    let lower = target.trim_start().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("about:")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
}

/// Forward queued non-web navigations (capsule:// / ato:// …) to the
/// app-level `NavigateToUrl` routing. Dispatches through this view's
/// own window; the loop ends when the window closes.
fn spawn_intercepted_nav_drain(
    cx: &mut Context<WebAppView>,
    queue: InterceptedNavQueue,
    host: gpui::AnyWindowHandle,
) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
            let drained: Vec<String> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                continue;
            }
            let mut live = true;
            aa.update(|cx: &mut gpui::App| {
                for url in drained {
                    tracing::info!(url = %url, "web app view: launch intent intercepted");
                    let result = host.update(cx, |_, window, cx| {
                        window.dispatch_action(Box::new(NavigateToUrl { url: url.clone() }), cx);
                    });
                    if result.is_err() {
                        live = false;
                        return;
                    }
                }
            });
            if !live {
                break;
            }
        }
    })
    .detach();
}

/// The `window.__ATO_DESKTOP__` init script: version/platform marker plus
/// the launch-handoff bridge. `launch({launch_id, capsule_ref})` returns a
/// Promise resolving to the Desktop's ack (`{accepted, reason?}`) — the
/// PWA keeps polling the launch itself until `accepted:true` comes back
/// (plan §4: no orphan launches). Only ids ride the channel; app_url /
/// tokens are rejected Rust-side by shape validation.
fn desktop_init_script() -> String {
    format!(
        r#"window.__ATO_DESKTOP__ = {{ version: "{version}", platform: "{platform}" }};
window.__ATO_DESKTOP_PENDING__ = (function() {{
    var pending = {{}};
    var next = 1;
    return {{
        register: function(resolve) {{ var id = next++; pending[id] = resolve; return id; }},
        resolve: function(id, result) {{
            var cb = pending[id];
            if (cb) {{ delete pending[id]; cb(result); }}
        }}
    }};
}})();
window.__ATO_DESKTOP__.launch = function(payload) {{
    return new Promise(function(resolve) {{
        var launchId = payload && payload.launch_id;
        var capsuleRef = (payload && payload.capsule_ref) || "";
        if (typeof launchId !== "string" || launchId === "") {{
            resolve({{ accepted: false, reason: "missing_launch_id" }});
            return;
        }}
        try {{
            var id = window.__ATO_DESKTOP_PENDING__.register(resolve);
            window.ipc.postMessage(JSON.stringify({{
                kind: "launch", id: id, launch_id: launchId, capsule_ref: String(capsuleRef)
            }}));
        }} catch (e) {{
            resolve({{ accepted: false, reason: "ipc_unavailable" }});
        }}
    }});
}};"#,
        version = env!("CARGO_PKG_VERSION"),
        platform = std::env::consts::OS,
    )
}

#[derive(Deserialize)]
struct RawIpcLaunchMessage {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    launch_id: Option<String>,
    #[serde(default)]
    capsule_ref: Option<String>,
}

/// Parse one raw IPC body into a queued launch request. Returns `None`
/// for non-launch traffic (silently ignored — this WebView has no other
/// IPC protocol today, but shims may post noise). Pure — safe to call
/// from the wry IPC thread. Validation is fail-closed: a malformed
/// launch_id / capsule_ref yields an `accepted:false` outcome, never a
/// partial registration.
fn parse_ipc_launch_message(body: &str) -> Option<IpcLaunchRequest> {
    let raw: RawIpcLaunchMessage = serde_json::from_str(body).ok()?;
    if raw.kind != "launch" {
        return None;
    }
    let request_id = raw.id?;
    let outcome = validate_launch_payload(raw.launch_id.as_deref(), raw.capsule_ref.as_deref());
    Some(IpcLaunchRequest {
        request_id,
        outcome,
    })
}

fn validate_launch_payload(
    launch_id: Option<&str>,
    capsule_ref: Option<&str>,
) -> Result<LaunchHandoff, String> {
    let launch_id = launch_id.unwrap_or_default();
    if !crate::launch_tracker::is_valid_launch_id(launch_id) {
        return Err("invalid_launch_id".to_string());
    }
    let capsule_ref = capsule_ref.unwrap_or_default();
    if !capsule_ref.is_empty() && !crate::launch_tracker::is_valid_capsule_ref(capsule_ref) {
        return Err("invalid_capsule_ref".to_string());
    }
    Ok(LaunchHandoff {
        launch_id: launch_id.to_string(),
        capsule_ref: capsule_ref.to_string(),
    })
}

/// The `evaluate_script` ack resolving one pending `launch()` promise.
fn pending_resolve_script(request_id: u64, accepted: bool, reason: Option<&str>) -> String {
    let result = match reason {
        Some(reason) => serde_json::json!({ "accepted": accepted, "reason": reason }),
        None => serde_json::json!({ "accepted": accepted }),
    };
    format!(
        "window.__ATO_DESKTOP_PENDING__ && window.__ATO_DESKTOP_PENDING__.resolve({request_id}, {result});"
    )
}

/// Drain queued `launch()` handoffs on the GPUI main thread: register
/// accepted launches in the [`crate::launch_tracker`] and ack the page
/// (same 50ms cadence as the intercepted-nav drain). Ends when this view
/// is dropped (window closed).
fn spawn_ipc_launch_drain(cx: &mut Context<WebAppView>, queue: IpcLaunchQueue) {
    let weak = cx.entity().downgrade();
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
            let drained: Vec<IpcLaunchRequest> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                // Cheap liveness probe so the loop ends with the view.
                if aa.update(|_cx| weak.upgrade().is_none()) {
                    break;
                }
                continue;
            }
            let mut live = true;
            aa.update(|cx: &mut gpui::App| {
                for request in drained {
                    let (accepted, reason) = match request.outcome {
                        Ok(handoff) => {
                            tracing::info!(
                                launch_id = %handoff.launch_id,
                                "web app view: launch handoff accepted"
                            );
                            crate::launch_tracker::register_launch(
                                cx,
                                handoff.launch_id,
                                handoff.capsule_ref,
                            );
                            (true, None)
                        }
                        Err(reason) => {
                            tracing::warn!(
                                reason = %reason,
                                "web app view: launch handoff rejected"
                            );
                            (false, Some(reason))
                        }
                    };
                    let script =
                        pending_resolve_script(request.request_id, accepted, reason.as_deref());
                    let ack = weak.update(cx, |view, _cx| {
                        if let Some(webview) = view.webview.as_ref()
                            && let Err(error) = webview.evaluate_script(&script)
                        {
                            tracing::warn!(error = %error, "launch handoff ack failed");
                        }
                    });
                    if ack.is_err() {
                        live = false;
                        return;
                    }
                }
            });
            if !live {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_launch_message_accepts_valid_payload() {
        let req = parse_ipc_launch_message(
            r#"{"kind":"launch","id":3,"launch_id":"lch_01KW","capsule_ref":"community/hello-capsule"}"#,
        )
        .unwrap();
        assert_eq!(req.request_id, 3);
        assert_eq!(
            req.outcome,
            Ok(LaunchHandoff {
                launch_id: "lch_01KW".to_string(),
                capsule_ref: "community/hello-capsule".to_string(),
            })
        );
    }

    #[test]
    fn ipc_launch_message_accepts_missing_capsule_ref() {
        let req =
            parse_ipc_launch_message(r#"{"kind":"launch","id":1,"launch_id":"lch_1"}"#).unwrap();
        assert_eq!(
            req.outcome,
            Ok(LaunchHandoff {
                launch_id: "lch_1".to_string(),
                capsule_ref: String::new(),
            })
        );
    }

    #[test]
    fn ipc_launch_message_rejects_url_and_token_shaped_ids() {
        for bad in [
            r#"{"kind":"launch","id":1,"launch_id":"https://evil.example/app"}"#,
            r#"{"kind":"launch","id":1,"launch_id":"eyJhbGci.eyJzdWIi.sig"}"#,
            r#"{"kind":"launch","id":1,"launch_id":""}"#,
            r#"{"kind":"launch","id":1}"#,
        ] {
            let req = parse_ipc_launch_message(bad).unwrap();
            assert_eq!(req.outcome, Err("invalid_launch_id".to_string()), "{bad}");
        }
        let bad_ref = parse_ipc_launch_message(
            r#"{"kind":"launch","id":1,"launch_id":"lch_1","capsule_ref":"https://evil.example"}"#,
        )
        .unwrap();
        assert_eq!(bad_ref.outcome, Err("invalid_capsule_ref".to_string()));
    }

    #[test]
    fn ipc_non_launch_traffic_is_ignored() {
        assert!(parse_ipc_launch_message(r#"{"__ato_ready__":true}"#).is_none());
        assert!(parse_ipc_launch_message("not json").is_none());
        assert!(parse_ipc_launch_message(r#"{"kind":"other","id":1}"#).is_none());
        // launch-kind without a request id can never be acked — dropped.
        assert!(
            parse_ipc_launch_message(r#"{"kind":"launch","launch_id":"lch_1"}"#).is_none()
        );
    }

    #[test]
    fn pending_resolve_script_escapes_reason_as_json() {
        assert_eq!(
            pending_resolve_script(7, true, None),
            "window.__ATO_DESKTOP_PENDING__ && window.__ATO_DESKTOP_PENDING__.resolve(7, {\"accepted\":true});"
        );
        let script = pending_resolve_script(8, false, Some("bad\"</script>"));
        assert!(script.contains("\"accepted\":false"));
        assert!(script.contains("bad\\\"</script>"));
    }

    #[test]
    fn desktop_init_script_defines_marker_and_launch_bridge() {
        let script = desktop_init_script();
        assert!(script.contains("window.__ATO_DESKTOP__ = {"));
        assert!(script.contains("window.__ATO_DESKTOP__.launch = function"));
        assert!(script.contains("window.__ATO_DESKTOP_PENDING__"));
        assert!(script.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn web_schemes_stay_in_view_and_intents_leave() {
        assert!(is_web_navigation("https://abc.app.ato.run/"));
        assert!(is_web_navigation("about:blank"));
        assert!(!is_web_navigation("capsule://community/hello-capsule"));
        assert!(!is_web_navigation("ato://open?handle=x"));
    }
}
