//! Auth Login Window — opens the Ato auth URL in an embedded Wry WebView
//! instead of the user's default external browser.
//!
//! Flow:
//! 1. Launch `ato login --desktop-webview` as a child process.
//! 2. Read the first NDJSON line (`desktop_login_started`) from stdout to
//!    get the `login_url`.
//! 3. Open a Wry WebView window loading that URL.
//! 4. A background thread watches the child process stdout for
//!    `desktop_login_completed` or `desktop_login_failed`.
//! 5. On completion: close this window and refresh the Dock.
//!    On failure / user close: kill the child process, close the window,
//!    and re-open the Dock so it shows the login page again.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context as GpuiContext, IntoElement,
    Render, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use serde::Deserialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::orchestrator::resolve_ato_binary;

// ── Global slot ───────────────────────────────────────────────────────────────

/// Tracks the single open AuthLoginWindow (if any).
#[derive(Default)]
pub struct AuthLoginWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for AuthLoginWindowSlot {}

// ── GPUI view ─────────────────────────────────────────────────────────────────

/// Lightweight GPUI view keeping the Wry WebView alive.
pub struct AuthLoginWebView {
    _webview: WebView,
}

impl Render for AuthLoginWebView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut GpuiContext<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xffffff))
    }
}

// ── NDJSON event types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DesktopLoginEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    login_url: Option<String>,
    #[serde(default)]
    publisher_handle: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Launch `ato login --desktop-webview`, read the start event, and open
/// an embedded WebView window for the OAuth flow.
///
/// Call this from `DockCommand::Login` instead of spawning bare `ato login`.
pub fn open_auth_login_window(cx: &mut App) -> Result<()> {
    // Only one login window at a time.
    if let Some(slot) = cx.try_global::<AuthLoginWindowSlot>() {
        if let Some(handle) = slot.0 {
            let result = handle.update(cx, |_, window, _| window.activate_window());
            if result.is_ok() {
                return Ok(());
            }
            cx.set_global(AuthLoginWindowSlot(None));
        }
    }

    let ato_bin = resolve_ato_binary().context("ato binary not found")?;

    // ── Launch the CLI subprocess ─────────────────────────────────────────────
    let mut child: Child = Command::new(&ato_bin)
        .arg("login")
        .arg("--desktop-webview")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .context("failed to spawn ato login --desktop-webview")?;

    let stdout = child.stdout.take().context("no stdout from child")?;
    let mut reader = BufReader::new(stdout);

    // Read the first line — must be `desktop_login_started`.
    let mut first_line = String::new();
    reader
        .read_line(&mut first_line)
        .context("failed to read start event from ato login")?;

    let event: DesktopLoginEvent = serde_json::from_str(first_line.trim())
        .context("invalid NDJSON start event from ato login")?;

    if event.kind != "desktop_login_started" {
        let msg = event.message.unwrap_or_else(|| first_line.trim().to_string());
        anyhow::bail!("ato login --desktop-webview: {}", msg);
    }

    let login_url = event
        .login_url
        .context("desktop_login_started missing login_url")?;

    // ── Open the GPUI window with the embedded WebView ─────────────────────────
    let win_size = size(px(900.0), px(700.0));
    let bounds = match cx.primary_display() {
        Some(d) => {
            let db = d.bounds();
            let left = db.origin.x + (db.size.width - win_size.width) / 2.0;
            let top = db.origin.y + px(80.0);
            Bounds { origin: gpui::point(left, top), size: win_size }
        }
        None => Bounds::centered(None, win_size, cx),
    };

    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let handle = cx.open_window(options, move |window, cx| {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };

        let webview = WebViewBuilder::new()
            .with_url(&login_url)
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for AuthLoginWindow");

        let view = cx.new(|_cx| AuthLoginWebView { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;

    cx.set_global(AuthLoginWindowSlot(Some(*handle)));

    // ── Background watcher using GPUI executors ────────────────────────────────
    // `be.spawn` runs the blocking I/O on background threads (requires Send).
    // `fe.spawn` schedules the UI update on the main GPUI thread (non-Send ok).
    // `aa` (AsyncApp, non-Send) is only used inside `fe.spawn`.
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let completion = be
            .spawn(async move { watch_login_completion(reader, child) })
            .await;
        let _ = aa.update(|cx| {
            on_login_completion(cx, completion);
        });
    })
    .detach();

    Ok(())
}

// ── Completion result ─────────────────────────────────────────────────────────

enum LoginCompletion {
    Success { publisher_handle: Option<String> },
    Failure { message: String },
}

/// Reads remaining stdout from the child and waits for it to exit.
fn watch_login_completion(
    reader: BufReader<impl std::io::Read>,
    mut child: Child,
) -> LoginCompletion {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<DesktopLoginEvent>(line.trim()) else {
            continue;
        };
        match event.kind.as_str() {
            "desktop_login_completed" => {
                let _ = child.wait();
                return LoginCompletion::Success {
                    publisher_handle: event.publisher_handle,
                };
            }
            "desktop_login_failed" => {
                let _ = child.wait();
                return LoginCompletion::Failure {
                    message: event.message.unwrap_or_else(|| "login failed".to_string()),
                };
            }
            _ => {}
        }
    }

    // Process exited without a completion event.
    let exit_status = child.wait();
    match exit_status {
        Ok(s) if s.success() => LoginCompletion::Success {
            publisher_handle: None,
        },
        Ok(s) => LoginCompletion::Failure {
            message: format!("ato login exited with status {}", s),
        },
        Err(e) => LoginCompletion::Failure {
            message: format!("waiting for ato login failed: {}", e),
        },
    }
}

/// Called on the GPUI thread after the child process finishes.
fn on_login_completion(cx: &mut App, result: LoginCompletion) {
    // Close the login window.
    cx.set_global(AuthLoginWindowSlot(None));

    match result {
        LoginCompletion::Success { publisher_handle } => {
            tracing::info!(
                publisher_handle = publisher_handle.as_deref().unwrap_or("(unknown)"),
                "Desktop login completed successfully"
            );
            // Refresh the Dock so it picks up the new identity.
            cx.set_global(crate::window::dock::DockWindowSlot(None));
            let _ = crate::window::dock::open_dock_window(cx);
        }
        LoginCompletion::Failure { message } => {
            tracing::warn!(message, "Desktop login failed or was cancelled");
            // Re-open the Dock so the user sees the login page again.
            cx.set_global(crate::window::dock::DockWindowSlot(None));
            let _ = crate::window::dock::open_dock_window(cx);
        }
    }
}
