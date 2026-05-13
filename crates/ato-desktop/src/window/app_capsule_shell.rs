//! `AppCapsuleShell` — per-AppWindow capsule session host.
//!
//! Each `AppWindow` spawned for a `GuestRoute::CapsuleHandle` owns exactly
//! one `AppCapsuleShell`. The shell:
//!
//!   1. Loads the per-handle secrets from `~/.ato/secrets.json`.
//!   2. Spawns a background thread that calls the blocking
//!      `orchestrator::resolve_and_start_guest` → `ato app session start`.
//!   3. Polls via a foreground timer task; when the result arrives, creates
//!      a Wry `WebView` as a native child of the GPUI window pointing at
//!      the running capsule's `local_url`.
//!   4. On success: closes the boot wizard window and shows a transparent
//!      backdrop (the WebView floats on top as an OS child window).
//!   5. On failure: shows an actionable error surface.
//!   6. On window close / `Drop`: stops the running session via
//!      `orchestrator::stop_guest_session`.
//!   7. Handles resize by updating WebView bounds whenever the GPUI window
//!      changes size.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, hsla, px, App, Context, FontWeight, IntoElement, Pixels, Render,
    SharedString, Size,
};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::orchestrator::{GuestLaunchSession, LaunchError};
use crate::window::launch_window::BootWindowSlot;

// ── state ──────────────────────────────────────────────────────────────────

enum CapsuleBootState {
    Booting,
    Ready { session: Box<GuestLaunchSession> },
    Failed { error: String },
}

// ── entity ─────────────────────────────────────────────────────────────────

pub struct AppCapsuleShell {
    handle: String,
    boot_state: CapsuleBootState,
    webview: Option<WebView>,
    /// Result delivered from the background launch thread.
    pending_result: Option<Result<GuestLaunchSession, LaunchError>>,
    /// Cached window size, used for WebView bounds and resize detection.
    window_size: Size<Pixels>,
    /// Shared with the background thread; set true when the user aborts
    /// (AbortBoot or window close) so a late-arriving Ok(session) is
    /// immediately stopped rather than displayed.
    abort_flag: Arc<AtomicBool>,
}

impl AppCapsuleShell {
    pub fn new(
        handle: String,
        configs: Vec<(String, String)>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let abort_flag = Arc::new(AtomicBool::new(false));

        // Load per-handle secrets from the persistent store on disk.
        let secret_store = crate::config::load_secrets();
        let secrets: Vec<_> = secret_store
            .secrets_for_capsule(&handle)
            .into_iter()
            .cloned()
            .collect();

        // Spawn background thread for the blocking orchestration call.
        let (tx, rx) = std::sync::mpsc::channel();
        let handle_clone = handle.clone();
        let abort_clone = Arc::clone(&abort_flag);
        std::thread::spawn(move || {
            let result =
                crate::orchestrator::resolve_and_start_guest(&handle_clone, &secrets, &configs);
            // If already aborted and the session started, stop it immediately.
            if abort_clone.load(Ordering::Acquire) {
                if let Ok(ref session) = result {
                    let sid = session.session_id.clone();
                    let _ = crate::orchestrator::stop_guest_session(&sid);
                }
                return; // don't send — entity is likely gone
            }
            let _ = tx.send(result);
        });

        // Foreground polling task: wake GPUI when the result arrives.
        let entity = cx.entity().downgrade();
        let abort_poll = Arc::clone(&abort_flag);
        let async_app = cx.to_async();
        async_app
            .foreground_executor()
            .spawn({
                let be = async_app.background_executor().clone();
                let aa = async_app.clone();
                async move {
                    loop {
                        be.timer(Duration::from_millis(100)).await;
                        match rx.try_recv() {
                            Ok(result) => {
                                aa.update(|cx: &mut App| {
                                    // Close the boot wizard and clear the slot.
                                    close_boot_window(cx);

                                    match entity.upgrade() {
                                        Some(entity) => {
                                            entity.update(cx, |shell, cx| {
                                                shell.pending_result = Some(result);
                                                cx.notify();
                                            });
                                        }
                                        None => {
                                            // AppWindow was closed before launch
                                            // finished; stop any started session.
                                            if let Ok(session) = result {
                                                let sid = session.session_id.clone();
                                                std::thread::spawn(move || {
                                                    let _ = crate::orchestrator::stop_guest_session(
                                                        &sid,
                                                    );
                                                });
                                            }
                                        }
                                    }
                                });
                                break;
                            }
                            Err(TryRecvError::Disconnected) => {
                                // Thread aborted before sending (abort_flag was set).
                                aa.update(|cx: &mut App| {
                                    close_boot_window(cx);
                                });
                                break;
                            }
                            Err(TryRecvError::Empty) => {
                                if abort_poll.load(Ordering::Acquire) {
                                    // Abort flagged before result arrived; the
                                    // background thread will stop the session.
                                    aa.update(|cx: &mut App| {
                                        close_boot_window(cx);
                                    });
                                    break;
                                }
                            }
                        }
                    }
                }
            })
            .detach();

        Self {
            handle,
            boot_state: CapsuleBootState::Booting,
            webview: None,
            pending_result: None,
            window_size: win_size,
            abort_flag,
        }
    }

    /// Signal the background thread to stop (abort case). The `abort_flag`
    /// is also set in `Drop`, so calling this explicitly is optional — it
    /// exists as a convenience for callers that want to abort before the
    /// entity is dropped (e.g., programmatic window close before full Drop).
    #[allow(dead_code)]
    pub fn abort(&self) {
        self.abort_flag.store(true, Ordering::Release);
    }

    /// Process a result that arrived from the background thread.
    /// Called from `render` when `pending_result` is `Some`.
    fn process_pending_result(&mut self, window: &mut gpui::Window) {
        let Some(result) = self.pending_result.take() else {
            return;
        };
        match result {
            Ok(session) => {
                let base = session.local_url.as_deref().unwrap_or("about:blank");
                let url = match session.frontend_url_path() {
                    Some(path) => format!("{}{}", base.trim_end_matches('/'), path),
                    None => base.to_string(),
                };
                let win_size = window.bounds().size;
                let w = f32::from(win_size.width) as u32;
                let h = f32::from(win_size.height) as u32;
                match WebViewBuilder::new()
                    .with_url(&url)
                    .with_bounds(Rect {
                        position: LogicalPosition::new(0i32, 0i32).into(),
                        size: LogicalSize::new(w, h).into(),
                    })
                    .build_as_child(window)
                {
                    Ok(webview) => {
                        tracing::info!(
                            handle = %self.handle,
                            url = %url,
                            session_id = %session.session_id,
                            "AppCapsuleShell: WebView created for running session"
                        );
                        self.webview = Some(webview);
                        self.window_size = win_size;
                        self.boot_state = CapsuleBootState::Ready { session: Box::new(session) };
                    }
                    Err(err) => {
                        // Session started but WebView failed; stop the session.
                        let sid = session.session_id.clone();
                        std::thread::spawn(move || {
                            let _ = crate::orchestrator::stop_guest_session(&sid);
                        });
                        self.boot_state = CapsuleBootState::Failed {
                            error: format!("WebView creation failed: {err}"),
                        };
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    handle = %self.handle,
                    error = %err,
                    "AppCapsuleShell: capsule launch failed"
                );
                self.boot_state = CapsuleBootState::Failed {
                    error: describe_launch_error(&err),
                };
            }
        }
    }

    /// Resize the child WebView when the GPUI window bounds change.
    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
        let Some(ref webview) = self.webview else {
            return;
        };
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        let w = f32::from(current.width) as u32;
        let h = f32::from(current.height) as u32;
        let _ = webview.set_bounds(Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(w, h).into(),
        });
        self.window_size = current;
    }
}

impl Drop for AppCapsuleShell {
    fn drop(&mut self) {
        // Signal the background thread to not display the session if it
        // arrives after the entity is gone.
        self.abort_flag.store(true, Ordering::Release);
        // If a session was already running, stop it.
        if let CapsuleBootState::Ready { session } = &self.boot_state {
            let sid = session.session_id.clone();
            std::thread::spawn(move || {
                if let Err(err) = crate::orchestrator::stop_guest_session(&sid) {
                    tracing::warn!(
                        session_id = %sid,
                        error = %err,
                        "AppCapsuleShell drop: stop_guest_session failed"
                    );
                } else {
                    tracing::info!(
                        session_id = %sid,
                        "AppCapsuleShell drop: session stopped"
                    );
                }
            });
        }
    }
}

impl Render for AppCapsuleShell {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.process_pending_result(window);
        self.sync_webview_bounds(window);
        match &self.boot_state {
            CapsuleBootState::Booting => render_booting(&self.handle),
            CapsuleBootState::Ready { .. } => {
                // The Wry WebView is positioned as a native OS child window
                // floating above this div; the div provides a dark backdrop
                // visible during page load and in transparent regions.
                div().size_full().bg(hsla(0.0, 0.0, 0.06, 1.0)).into_any()
            }
            CapsuleBootState::Failed { error } => render_error(&self.handle, error),
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn close_boot_window(cx: &mut App) {
    let slot = cx
        .try_global::<BootWindowSlot>()
        .and_then(|s| s.boot_window);
    if let Some(handle) = slot {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        // Clear both fields — once the launch result arrives, AbortBoot
        // is no longer applicable (boot window is gone).
        cx.set_global(BootWindowSlot::default());
        tracing::info!("AppCapsuleShell: boot wizard closed");
    }
}

fn describe_launch_error(err: &LaunchError) -> String {
    match err {
        LaunchError::PreflightAggregate { handle, requirements, .. } => {
            let consent_count = requirements
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        capsule_core::interactive_resolution::InteractiveResolutionKind::ConsentRequired { .. }
                    )
                })
                .count();
            let secret_count = requirements.len() - consent_count;
            let mut parts = Vec::new();
            if consent_count > 0 {
                parts.push(format!(
                    "{consent_count} consent(s) pending — run: ato internal consent approve-execution-plan"
                ));
            }
            if secret_count > 0 {
                parts.push(format!(
                    "{secret_count} required secret(s) — run: ato app config set {handle}"
                ));
            }
            format!("Launch prerequisites not met:\n{}", parts.join("\n"))
        }
        LaunchError::MissingConsent { handle, .. } => {
            format!(
                "Capsule consent required.\nRun: ato internal consent approve-execution-plan \
                 --handle {handle}"
            )
        }
        LaunchError::MissingConfig { handle, fields, .. } => {
            let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
            format!(
                "Missing required config: {}\nRun: ato app config set {}",
                names.join(", "),
                handle
            )
        }
        LaunchError::Other(msg) => msg.clone(),
    }
}

fn render_booting(handle: &str) -> gpui::AnyElement {
    use gpui::{rgb, ParentElement, Styled};

    div()
        .size_full()
        .bg(hsla(0.0, 0.0, 0.08, 1.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            div()
                .text_color(rgb(0x60a5fa))
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(14.0))
                .child(SharedString::from("Starting capsule…")),
        )
        .child(
            div()
                .text_color(rgb(0x6b7280))
                .text_size(px(12.0))
                .child(SharedString::from(handle.to_string())),
        )
        .into_any()
}

fn render_error(handle: &str, error: &str) -> gpui::AnyElement {
    use gpui::{rgb, ParentElement, Styled};

    div()
        .size_full()
        .bg(hsla(0.0, 0.0, 0.08, 1.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .p_8()
        .child(
            div()
                .text_color(rgb(0xf87171))
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(14.0))
                .child(SharedString::from("Launch failed")),
        )
        .child(
            div()
                .text_color(rgb(0x9ca3af))
                .text_size(px(12.0))
                .child(SharedString::from(handle.to_string())),
        )
        .child(
            div()
                .text_color(rgb(0xd1d5db))
                .text_size(px(12.0))
                .max_w(px(520.0))
                .child(SharedString::from(error.to_string())),
        )
        .into_any()
}
