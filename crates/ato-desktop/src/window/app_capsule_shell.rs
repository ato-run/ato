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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Context, FontWeight, IntoElement, Pixels, Render, SharedString, Size, WeakEntity, div,
    hsla, px,
};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{PageLoadEvent, Rect, WebView, WebViewBuilder};

use ato_protocol::handle::CapsuleDisplayStrategy;

use crate::automation::AutomationHost;
use crate::automation::command::PendingAutomationRequest;
use crate::orchestrator::{DesktopLaunchInput, GuestLaunchSession, LaunchError};
use crate::state::session::{
    CapsuleLaunchContext, CapsuleOpenSource, CapsuleSession, SessionClient, SessionClientId,
    SessionClientKind, SessionClientState, SessionRegistry,
};
use crate::window::content_windows::{
    CapsuleWindowContext, CapsuleWindowStatus, OpenContentWindows,
};
use crate::window::launch_window::{BootWindowSlot, LaunchWindowShell, PendingBootShell};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

// ── state ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum CapsuleBootInput {
    Start {
        handle: String,
        configs: Vec<(String, String)>,
    },
    Launch {
        input: DesktopLaunchInput,
        configs: Vec<(String, String)>,
    },
    MaterializedRestart {
        handle: String,
        record_path: PathBuf,
        configs: Vec<(String, String)>,
    },
    Ready {
        session: GuestLaunchSession,
        configs: Vec<(String, String)>,
    },
}

enum CapsuleBootState {
    Booting,
    Ready { session: Box<GuestLaunchSession> },
    Failed { error: String },
}

// ── entity ─────────────────────────────────────────────────────────────────

pub struct AppCapsuleShell {
    handle: String,
    launch_configs: Vec<(String, String)>,
    boot_state: CapsuleBootState,
    webview: Option<WebView>,
    content_window_id: Option<u64>,
    /// The URL actually loaded into `webview` once it is created. This is
    /// the *effective* URL — for `WebUrl` sessions it is the ato-netd stable
    /// ingress URL (`http://127.0.0.1:<port>/…`), which differs from the
    /// upstream `session_current_url`. Reported to MCP `browser_tabs` so the
    /// pane's URL matches what is really on screen (#370 review follow-up).
    automation_url: Option<String>,
    /// Result delivered from the background launch thread.
    pending_result: Option<Result<GuestLaunchSession, LaunchError>>,
    /// Cached window size, used for WebView bounds and resize detection.
    window_size: Size<Pixels>,
    /// Shared with the background thread; set true when the user aborts
    /// (AbortBoot or window close) so a late-arriving Ok(session) is
    /// immediately stopped rather than displayed.
    abort_flag: Arc<AtomicBool>,
    pub paste: WebViewPasteSupport,
    /// ato-netd stable ingress key registered for this shell's WebUrl session,
    /// if any. Deregistered on Drop.
    stable_ingress_key: Option<String>,
}

impl_focusable_via_paste!(AppCapsuleShell, paste);

impl WebViewPasteShell for AppCapsuleShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }
}

impl AppCapsuleShell {
    pub fn new_with_input(
        input: CapsuleBootInput,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        match input {
            CapsuleBootInput::Start { handle, configs } => Self::new(handle, configs, window, cx),
            CapsuleBootInput::Launch { input, configs } => {
                Self::new_with_launch_input(input, configs, window, cx)
            }
            CapsuleBootInput::MaterializedRestart {
                handle,
                record_path,
                configs,
            } => Self::new_from_materialized_record(handle, record_path, configs, window, cx),
            CapsuleBootInput::Ready { session, configs } => {
                Self::new_ready(session, configs, window, cx)
            }
        }
    }

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
        let secrets: Vec<_> = secret_store.secrets_for_capsule(&handle);

        // Spawn background thread for the blocking orchestration call.
        let (tx, rx) = std::sync::mpsc::channel();
        // Separate channel for fine-grained step progress from the orchestrator.
        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<u8>();

        // Read (and immediately clear) the boot shell weak entity set by
        // `open_boot_window`. Clearing prevents stale references leaking
        // to a subsequent launch that opens before this one's entity drops.
        let boot_shell_weak: Option<WeakEntity<LaunchWindowShell>> = cx
            .try_global::<PendingBootShell>()
            .and_then(|g| g.0.clone());
        cx.set_global(PendingBootShell(None));

        let handle_clone = handle.clone();
        let launch_configs = configs.clone();
        let configs_for_thread = configs.clone();
        let abort_clone = Arc::clone(&abort_flag);
        std::thread::spawn(move || {
            let prog = progress_tx;
            let result = crate::orchestrator::resolve_and_start_guest(
                &handle_clone,
                &secrets,
                &configs_for_thread,
                Some(Box::new(move |step| {
                    let _ = prog.send(step);
                })),
            );
            // For WebUrl sessions (e.g. OCI/Docker Compose), the CLI returns as
            // soon as the port is allocated, but the web server inside the
            // container may still be initializing. Opening the WebView too early
            // causes a thundering-herd: the browser loads the HTML (via our 503
            // auto-refresh), then immediately fires ~150 parallel sub-resource
            // requests that overwhelm the barely-started server → all 503.
            //
            // Probe the upstream directly until it responds to HTTP (up to 60s),
            // checking the abort flag on every iteration so a cancelled launch
            // exits within ~500 ms.
            if let Ok(ref session) = result
                && session.display_strategy == CapsuleDisplayStrategy::WebUrl
            {
                wait_for_session_upstream_ready(session, &abort_clone, Duration::from_secs(60));
            }
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
                        if crate::webview_init_guard::WebviewInitGuard::is_active() {
                            continue;
                        }

                        // Drain progress steps before checking the result so
                        // the boot wizard advances as the orchestrator works.
                        let steps: Vec<u8> = {
                            let mut v = Vec::new();
                            while let Ok(s) = progress_rx.try_recv() {
                                v.push(s);
                            }
                            v
                        };
                        if !steps.is_empty() {
                            aa.update(|cx: &mut App| {
                                if let Some(weak) = &boot_shell_weak
                                    && let Some(shell) = weak.upgrade()
                                {
                                    for step in steps {
                                        shell.update(cx, |s, _cx| {
                                            s.push_step(step);
                                            let msg = match step {
                                                0 => "Validating launch plan",
                                                1 => "Resolving capsule targets",
                                                2 => "Starting capsule session",
                                                3 => "Connecting to capsule endpoint",
                                                _ => "Processing launch step",
                                            };
                                            s.push_detail(msg);
                                        });
                                    }
                                }
                            });
                        }

                        match rx.try_recv() {
                            Ok(result) => {
                                aa.update(|cx: &mut App| {
                                    // Close the boot wizard and clear the slot.
                                    close_boot_window(cx);

                                    match entity.upgrade() {
                                        Some(entity) => {
                                            if let Some(weak) = &boot_shell_weak
                                                && let Some(shell) = weak.upgrade()
                                            {
                                                shell.update(cx, |s, _cx| match &result {
                                                    Ok(_) => s.push_detail(
                                                        "Capsule session started successfully",
                                                    ),
                                                    Err(err) => s.push_detail(&format!(
                                                        "Launch failed: {}",
                                                        describe_launch_error(err)
                                                    )),
                                                });
                                            }
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
            launch_configs,
            boot_state: CapsuleBootState::Booting,
            webview: None,
            content_window_id: None,
            automation_url: None,
            pending_result: None,
            window_size: win_size,
            abort_flag,
            paste: WebViewPasteSupport::new(cx),
            stable_ingress_key: None,
        }
    }

    pub fn new_with_launch_input(
        input: DesktopLaunchInput,
        configs: Vec<(String, String)>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = input.source_handle().to_string();
        let win_size = window.bounds().size;
        let abort_flag = Arc::new(AtomicBool::new(false));

        let secret_store = crate::config::load_secrets();
        let secrets: Vec<_> = secret_store.secrets_for_capsule(&handle);

        let (tx, rx) = std::sync::mpsc::channel();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<u8>();

        let boot_shell_weak: Option<WeakEntity<LaunchWindowShell>> = cx
            .try_global::<PendingBootShell>()
            .and_then(|g| g.0.clone());
        cx.set_global(PendingBootShell(None));

        let launch_configs = configs.clone();
        let configs_for_thread = configs.clone();
        let abort_clone = Arc::clone(&abort_flag);
        std::thread::spawn(move || {
            let prog = progress_tx;
            let result = crate::orchestrator::resolve_and_start_guest_with_input(
                &input,
                &secrets,
                &configs_for_thread,
                Some(Box::new(move |step| {
                    let _ = prog.send(step);
                })),
            );
            if let Ok(ref session) = result
                && session.display_strategy == CapsuleDisplayStrategy::WebUrl
            {
                wait_for_session_upstream_ready(session, &abort_clone, Duration::from_secs(60));
            }
            if abort_clone.load(Ordering::Acquire) {
                if let Ok(ref session) = result {
                    let sid = session.session_id.clone();
                    let _ = crate::orchestrator::stop_guest_session(&sid);
                }
                return;
            }
            let _ = tx.send(result);
        });

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
                        if crate::webview_init_guard::WebviewInitGuard::is_active() {
                            continue;
                        }

                        let steps: Vec<u8> = {
                            let mut v = Vec::new();
                            while let Ok(s) = progress_rx.try_recv() {
                                v.push(s);
                            }
                            v
                        };
                        if !steps.is_empty() {
                            let boot = boot_shell_weak.clone();
                            aa.update(move |cx| {
                                if let Some(shell) = boot.and_then(|w| w.upgrade()) {
                                    shell.update(cx, |shell, _| {
                                        for s in &steps {
                                            shell.push_step(*s);
                                        }
                                    });
                                }
                            });
                        }

                        match rx.try_recv() {
                            Ok(res) => {
                                if let Some(ent) = entity.upgrade() {
                                    aa.update(move |cx| {
                                        ent.update(cx, |shell, cx| {
                                            shell.pending_result = Some(res);
                                            cx.notify();
                                        });
                                    });
                                }
                                break;
                            }
                            Err(TryRecvError::Disconnected) => break,
                            Err(TryRecvError::Empty) => {
                                if abort_poll.load(Ordering::Acquire) {
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
            launch_configs,
            boot_state: CapsuleBootState::Booting,
            webview: None,
            content_window_id: None,
            automation_url: None,
            pending_result: None,
            window_size: win_size,
            abort_flag,
            paste: WebViewPasteSupport::new(cx),
            stable_ingress_key: None,
        }
    }

    pub fn new_from_materialized_record(
        handle: String,
        record_path: PathBuf,
        configs: Vec<(String, String)>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let abort_flag = Arc::new(AtomicBool::new(false));

        let secret_store = crate::config::load_secrets();
        let secrets: Vec<_> = secret_store.secrets_for_capsule(&handle);

        let (tx, rx) = std::sync::mpsc::channel();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<u8>();

        let boot_shell_weak: Option<WeakEntity<LaunchWindowShell>> = cx
            .try_global::<PendingBootShell>()
            .and_then(|g| g.0.clone());
        cx.set_global(PendingBootShell(None));

        let handle_clone = handle.clone();
        let record_path_clone = record_path.clone();
        let configs_for_thread = configs.clone();
        let abort_clone = Arc::clone(&abort_flag);
        std::thread::spawn(move || {
            let prog = progress_tx;
            let result = crate::orchestrator::resolve_and_start_guest_from_materialized_record(
                &handle_clone,
                &record_path_clone,
                &secrets,
                &configs_for_thread,
                Some(Box::new(move |step| {
                    let _ = prog.send(step);
                })),
            )
            .or_else(|err| {
                tracing::warn!(
                    error = %err,
                    handle = %handle_clone,
                    record_path = %record_path_clone.display(),
                    "materialized relaunch failed; falling back to cold launch"
                );
                crate::orchestrator::resolve_and_start_guest(
                    &handle_clone,
                    &secrets,
                    &configs_for_thread,
                    None,
                )
            });
            if let Ok(ref session) = result
                && session.display_strategy == CapsuleDisplayStrategy::WebUrl
            {
                wait_for_session_upstream_ready(session, &abort_clone, Duration::from_secs(60));
            }
            if abort_clone.load(Ordering::Acquire) {
                if let Ok(ref session) = result {
                    let sid = session.session_id.clone();
                    let _ = crate::orchestrator::stop_guest_session(&sid);
                }
                return;
            }
            let _ = tx.send(result);
        });

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
                        if crate::webview_init_guard::WebviewInitGuard::is_active() {
                            continue;
                        }

                        let steps: Vec<u8> = {
                            let mut v = Vec::new();
                            while let Ok(s) = progress_rx.try_recv() {
                                v.push(s);
                            }
                            v
                        };
                        if !steps.is_empty() {
                            aa.update(|cx: &mut App| {
                                if let Some(weak) = &boot_shell_weak
                                    && let Some(shell) = weak.upgrade()
                                {
                                    for step in steps {
                                        shell.update(cx, |s, _cx| {
                                            s.push_step(step);
                                            let msg = match step {
                                                0 => "Validating launch plan",
                                                1 => "Resolving capsule targets",
                                                2 => "Starting capsule session",
                                                3 => "Connecting to capsule endpoint",
                                                _ => "Processing launch step",
                                            };
                                            s.push_detail(msg);
                                        });
                                    }
                                }
                            });
                        }

                        match rx.try_recv() {
                            Ok(result) => {
                                aa.update(|cx: &mut App| {
                                    close_boot_window(cx);

                                    match entity.upgrade() {
                                        Some(entity) => {
                                            if let Some(weak) = &boot_shell_weak
                                                && let Some(shell) = weak.upgrade()
                                            {
                                                shell.update(cx, |s, _cx| match &result {
                                                    Ok(_) => s.push_detail(
                                                        "Capsule session started successfully",
                                                    ),
                                                    Err(err) => s.push_detail(&format!(
                                                        "Launch failed: {}",
                                                        describe_launch_error(err)
                                                    )),
                                                });
                                            }
                                            entity.update(cx, |shell, cx| {
                                                shell.pending_result = Some(result);
                                                cx.notify();
                                            });
                                        }
                                        None => {
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
                            Err(TryRecvError::Disconnected) => break,
                            Err(TryRecvError::Empty) => {
                                if abort_poll.load(Ordering::Acquire) {
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
            launch_configs: configs.clone(),
            boot_state: CapsuleBootState::Booting,
            webview: None,
            content_window_id: None,
            automation_url: None,
            pending_result: None,
            window_size: win_size,
            abort_flag,
            paste: WebViewPasteSupport::new(cx),
            stable_ingress_key: None,
        }
    }

    fn new_ready(
        session: GuestLaunchSession,
        configs: Vec<(String, String)>,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let handle = session.handle.clone();
        tracing::info!(
            handle = %handle,
            session_id = %session.session_id,
            "AppCapsuleShell::new_ready: created with pre-resolved session"
        );
        Self {
            handle,
            launch_configs: configs,
            boot_state: CapsuleBootState::Booting,
            webview: None,
            content_window_id: None,
            automation_url: None,
            pending_result: Some(Ok(session)),
            window_size: win_size,
            abort_flag: Arc::new(AtomicBool::new(false)),
            paste: WebViewPasteSupport::new(cx),
            stable_ingress_key: None,
        }
    }

    pub fn set_content_window_id(&mut self, window_id: u64) {
        self.content_window_id = Some(window_id);
    }

    /// Signal the background thread to stop (abort case). The `abort_flag`
    /// is also set in `Drop`, so calling this explicitly is optional — it
    /// exists as a convenience for callers that want to abort before the
    /// entity is dropped (e.g., programmatic window close before full Drop).
    #[allow(dead_code)]
    pub fn abort(&self) {
        self.abort_flag.store(true, Ordering::Release);
    }

    /// True once the guest WebView has been created (boot succeeded). Used
    /// by the Focus automation dispatcher to decide whether a registered
    /// guest pane can yet service browser commands (#370).
    pub fn has_webview(&self) -> bool {
        self.webview.is_some()
    }

    /// Session id of the running guest, if boot has completed.
    pub fn current_session_id(&self) -> Option<String> {
        match &self.boot_state {
            CapsuleBootState::Ready { session } => Some(session.session_id.clone()),
            _ => None,
        }
    }

    /// Best-effort current URL for automation / pane listing. Returns the
    /// URL actually loaded into the WebView once it exists (the effective
    /// ingress URL), so `browser_tabs` matches what's on screen. Falls back
    /// to the upstream `session_current_url` / `capsule://<handle>` form
    /// while still booting or on failure.
    pub fn current_url_for_automation(&self) -> String {
        if let Some(url) = &self.automation_url {
            return url.clone();
        }
        match &self.boot_state {
            CapsuleBootState::Ready { session } => session_current_url(session),
            _ => format!("capsule://{}", self.handle),
        }
    }

    /// Dispatch an MCP browser automation command to this shell's private
    /// guest WebView (#370). Mirrors the dock pane's
    /// `webview::dispatch_automation_command` path so guest capsules behave
    /// identically to the dock under MCP browser tools.
    pub fn dispatch_automation_request(
        &mut self,
        req: PendingAutomationRequest,
        pane_id: usize,
        automation: &AutomationHost,
    ) {
        match self.webview.as_ref() {
            Some(webview) => {
                crate::webview::dispatch_automation_command(req, webview, pane_id, automation);
            }
            None => {
                req.send(Err("guest capsule WebView is not ready".into()));
            }
        }
    }

    /// Process a result that arrived from the background thread.
    /// Called from `render` when `pending_result` is `Some`.
    fn process_pending_result(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(result) = self.pending_result.take() else {
            return;
        };
        match result {
            Ok(session) => {
                // Register the session in the SessionRegistry before creating
                // the WebView, so the session record exists even if WebView
                // creation fails (the process is already running).
                let launch_context = CapsuleLaunchContext {
                    handle_or_url: self.handle.clone(),
                    target: None,
                    launch_configs: self.launch_configs.clone(),
                    requested_client: SessionClientKind::AtoWindow,
                    source: CapsuleOpenSource::NavigateToUrl,
                };
                let capsule_session = CapsuleSession::from_launch_session(&session, launch_context);
                tracing::info!(
                    session_id = %session.session_id,
                    handle = %self.handle,
                    runtime_kind = "source",
                    window_id = ?self.content_window_id,
                    "app instance registered in desktop state (source)"
                );
                let registry = cx.global_mut::<SessionRegistry>();
                registry.register_session(capsule_session);
                let client = SessionClient {
                    client_id: SessionClientId::next(),
                    session_id: session.session_id.clone(),
                    client_kind: SessionClientKind::AtoWindow,
                    window_id: self.content_window_id,
                    pane_id: None,
                    state: SessionClientState::Attached,
                    attached_at: std::time::SystemTime::now(),
                    last_seen_at: std::time::SystemTime::now(),
                };
                registry.attach_client(client);

                let url = session_current_url(&session);
                // For WebUrl sessions, register a stable ato-netd ingress route
                // so the WebView URL is stable across backend restarts.
                let effective_url = if session.display_strategy == CapsuleDisplayStrategy::WebUrl {
                    let key = ato_net::stable_origin::logical_key_for_handle(&self.handle);
                    tracing::info!(
                        handle = %self.handle,
                        session_id = %session.session_id,
                        upstream_url = %url,
                        ingress_key = %key,
                        "AppCapsuleShell: registering ato-netd stable ingress"
                    );
                    match crate::netd::register_stable_ingress(&key, &url) {
                        Ok(port) => {
                            self.stable_ingress_key = Some(key);
                            let after_scheme = url
                                .trim_start_matches("http://")
                                .trim_start_matches("https://");
                            let path = after_scheme
                                .find('/')
                                .map(|i| &after_scheme[i..])
                                .unwrap_or("/");
                            format!("http://127.0.0.1:{port}{path}")
                        }
                        Err(err) => {
                            tracing::warn!(
                                handle = %self.handle,
                                error = %err,
                                "AppCapsuleShell: netd ingress registration failed, using direct URL"
                            );
                            url
                        }
                    }
                } else {
                    url
                };
                let win_size = window.bounds().size;
                let w = f32::from(win_size.width) as u32;
                let h = f32::from(win_size.height) as u32;
                let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
                let mut builder = WebViewBuilder::new()
                    .with_url(&effective_url)
                    .with_incognito(true)
                    .with_bounds(Rect {
                        position: LogicalPosition::new(0i32, 0i32).into(),
                        size: LogicalSize::new(w, h).into(),
                    });
                // Mark this guest pane loaded/unloaded for the Focus automation
                // dispatcher (#370), mirroring the dock WebView's handler. MCP
                // JS commands (snapshot/click/evaluate) are gated on
                // `is_page_loaded`, so without this they'd time out against
                // guest capsule panes.
                let automation_pane = self
                    .content_window_id
                    .map(crate::window::focus_guest_panes::focus_guest_pane_id);
                if let (Some(pane_id), Some(automation)) =
                    (automation_pane, cx.try_global::<AutomationHost>().cloned())
                {
                    // Start unloaded; the Finished event flips it to loaded.
                    automation.mark_page_unloaded(pane_id);
                    builder = builder.with_on_page_load_handler(move |event, _url| match event {
                        PageLoadEvent::Started => automation.mark_page_unloaded(pane_id),
                        PageLoadEvent::Finished => automation.mark_page_loaded(pane_id),
                    });
                }
                match builder.build_as_child(window) {
                    Ok(webview) => {
                        tracing::info!(
                            handle = %self.handle,
                            url = %effective_url,
                            session_id = %session.session_id,
                            "AppCapsuleShell: WebView created for running session"
                        );
                        self.webview = Some(webview);
                        self.automation_url = Some(effective_url.clone());
                        self.window_size = win_size;
                        self.boot_state = CapsuleBootState::Ready {
                            session: Box::new(session),
                        };
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

        // Deregister the stable ingress route if one was registered.
        //
        // `deregister_stable_ingress` issues a *synchronous* IPC round-trip to
        // ato-netd. The daemon can take up to ~60s to answer DeregisterIngress
        // while it drains in-flight ingress connections, so calling it inline
        // here blocks the GPUI main thread for the full duration — the closed
        // window stays on screen and the whole app freezes (measured: a 57.7s
        // hang on close). Deregistration is best-effort and idempotent (the key
        // is re-registered on the next launch), so fire-and-forget it on a
        // detached background thread instead of blocking window teardown.
        if let Some(key) = self.stable_ingress_key.take() {
            let window_id = self.content_window_id;
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                crate::netd::deregister_stable_ingress(&key);
                tracing::debug!(
                    ?window_id,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "deregistered stable ingress off the UI thread"
                );
            });
        }

        // Session lifecycle is now owned by the SessionRegistry.
        // on_window_closed in app.rs handles detach_client / stop_session_once
        // based on windowCloseBehavior. Drop is only a safety net: log
        // if a session exists but was NOT stopped by the close handler.
        if let Some(Ok(session)) = &self.pending_result {
            tracing::warn!(
                session_id = %session.session_id,
                window_id = ?self.content_window_id,
                "AppCapsuleShell drop: pending session was not consumed by close handler"
            );
        }
        if let CapsuleBootState::Ready { session } = &self.boot_state {
            let window_id = self.content_window_id;
            tracing::warn!(
                session_id = %session.session_id,
                ?window_id,
                "AppCapsuleShell drop: ready session was not detached by close handler"
            );
        }
    }
}

impl Render for AppCapsuleShell {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.process_pending_result(window, cx);
        self.sync_webview_bounds(window);
        publish_content_window_context(window, self, cx);
        let inner = match &self.boot_state {
            CapsuleBootState::Booting => render_booting(&self.handle),
            CapsuleBootState::Ready { .. } => {
                // The Wry WebView is positioned as a native OS child window
                // floating above this div; the div provides a dark backdrop
                // visible during page load and in transparent regions.
                div().size_full().bg(hsla(0.0, 0.0, 0.06, 1.0)).into_any()
            }
            CapsuleBootState::Failed { error } => render_error(&self.handle, error),
        };
        paste_render_wrap!(div().size_full().child(inner), cx, &self.paste.focus_handle)
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Outcome returned by [`wait_for_session_upstream_ready`].
#[derive(Debug, PartialEq)]
pub(crate) enum ProbeOutcome {
    /// Upstream answered HTTP within the timeout.
    Ready,
    /// Timeout elapsed without a successful probe.
    TimedOut,
    /// The `abort` flag was set; the probe stopped early.
    Aborted,
}

/// Probe the upstream for a `WebUrl` session until it responds to HTTP, the
/// `abort` flag fires, or `timeout` elapses.
///
/// **Probe URL selection** (first wins):
///   1. `session.healthcheck_url` — a dedicated health endpoint if present.
///   2. `session.local_url` + `frontend_url_path()` — the URL the WebView will open.
///   3. `session.local_url` — bare upstream base.
///
/// **"Ready" criterion:** any valid HTTP response (2xx / 3xx / 4xx all count).
/// The purpose is "server process is up and speaking HTTP", not "request is
/// functionally successful". A 302 redirect to `/auth` or a 401 Unauthorized
/// both confirm the server is running; only connection-refused / timeout means
/// it is not yet ready.
///
/// The loop checks `abort` before every poll iteration so cancelled launches
/// stop within one `poll_interval` (≈ 500 ms) rather than blocking for the
/// full timeout.
pub(crate) fn wait_for_session_upstream_ready(
    session: &GuestLaunchSession,
    abort: &AtomicBool,
    timeout: Duration,
) -> ProbeOutcome {
    let probe_url = if let Some(ref hc) = session.healthcheck_url {
        crate::netd::normalize_upstream_url(hc).into_owned()
    } else {
        let base = session.local_url.as_deref().unwrap_or("about:blank");
        let with_path = match session.frontend_url_path() {
            Some(path) => format!("{}{}", base.trim_end_matches('/'), path),
            None => base.to_string(),
        };
        crate::netd::normalize_upstream_url(&with_path).into_owned()
    };

    let deadline = std::time::Instant::now() + timeout;
    let poll_interval = Duration::from_millis(500);

    tracing::debug!(probe_url = %probe_url, "starting upstream readiness probe");

    loop {
        if abort.load(Ordering::Acquire) {
            tracing::debug!(probe_url = %probe_url, "upstream probe aborted");
            return ProbeOutcome::Aborted;
        }
        if capsule::state::session::healthcheck::http_is_responsive(&probe_url, Duration::from_millis(800))
        {
            tracing::info!(probe_url = %probe_url, "upstream HTTP readiness probe passed");
            return ProbeOutcome::Ready;
        }
        tracing::debug!(probe_url = %probe_url, "upstream not ready yet, retrying");
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                probe_url = %probe_url,
                timeout_secs = timeout.as_secs(),
                "upstream readiness probe timed out, opening WebView anyway"
            );
            return ProbeOutcome::TimedOut;
        }
        std::thread::sleep(poll_interval);
    }
}

fn session_current_url(session: &GuestLaunchSession) -> String {
    let base = session.local_url.as_deref().unwrap_or("about:blank");
    match session.frontend_url_path() {
        Some(path) => format!("{}{}", base.trim_end_matches('/'), path),
        None => base.to_string(),
    }
}

fn publish_content_window_context(
    _window: &mut gpui::Window,
    shell: &AppCapsuleShell,
    cx: &mut Context<AppCapsuleShell>,
) {
    let Some(window_id) = shell.content_window_id else {
        return;
    };
    let context = match &shell.boot_state {
        CapsuleBootState::Booting => Some(CapsuleWindowContext {
            title: short_title(&shell.handle),
            handle: shell.handle.clone(),
            canonical_handle: None,
            session_id: None,
            current_url: format!("capsule://{}", shell.handle),
            local_url: None,
            snapshot_label: None,
            trust_state: "pending".to_string(),
            runtime_label: None,
            display_strategy: None,
            capabilities: Vec::new(),
            log_path: None,
            status: CapsuleWindowStatus::Starting,
            restricted: false,
            error_message: None,
        }),
        CapsuleBootState::Ready { session } => Some(CapsuleWindowContext {
            title: short_title(
                session
                    .canonical_handle
                    .as_deref()
                    .unwrap_or(session.handle.as_str()),
            ),
            handle: session.handle.clone(),
            canonical_handle: session.canonical_handle.clone(),
            session_id: Some(session.session_id.clone()),
            current_url: session_current_url(session),
            local_url: session.local_url.clone(),
            snapshot_label: session.snapshot_label.clone(),
            trust_state: session.trust_state.clone(),
            runtime_label: Some(if !session.target_label.is_empty() {
                session.target_label.clone()
            } else {
                session.runtime.runtime.clone().unwrap_or_default()
            }),
            display_strategy: Some(session.display_strategy.as_str().to_string()),
            capabilities: session.capabilities.clone(),
            log_path: session
                .log_path
                .as_ref()
                .map(|path| path.display().to_string()),
            status: CapsuleWindowStatus::Ready,
            restricted: session.restricted,
            error_message: None,
        }),
        CapsuleBootState::Failed { error } => Some(CapsuleWindowContext {
            title: short_title(&shell.handle),
            handle: shell.handle.clone(),
            canonical_handle: None,
            session_id: None,
            current_url: format!("capsule://{}", shell.handle),
            local_url: None,
            snapshot_label: None,
            trust_state: "error".to_string(),
            runtime_label: None,
            display_strategy: None,
            capabilities: Vec::new(),
            log_path: None,
            status: CapsuleWindowStatus::Failed,
            restricted: false,
            error_message: Some(error.clone()),
        }),
    };
    cx.global_mut::<OpenContentWindows>()
        .set_capsule_context(window_id, context);
}

fn short_title(handle: &str) -> String {
    handle
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(handle)
        .to_string()
}

fn close_boot_window(cx: &mut App) {
    let slot = cx
        .try_global::<BootWindowSlot>()
        .and_then(|s| s.boot_window);
    if let Some(handle) = slot {
        // Diagnostic for #370: confirm the window being removed here is the
        // boot wizard, not the AppWindow that hosts the guest WebView.
        tracing::info!(
            window_id = handle.window_id().as_u64(),
            "close_boot_window removing boot window"
        );
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        // Clear both fields — once the launch result arrives, AbortBoot
        // is no longer applicable (boot window is gone).
        cx.set_global(BootWindowSlot::default());
        tracing::info!("AppCapsuleShell: boot wizard closed");
    }
}

pub(crate) fn describe_launch_error(err: &LaunchError) -> String {
    match err {
        LaunchError::PreflightAggregate {
            handle,
            requirements,
            ..
        } => {
            use capsule::interactive_resolution::InteractiveResolutionKind;
            let consent_count = requirements
                .iter()
                .filter(|e| matches!(e.kind, InteractiveResolutionKind::ConsentRequired { .. }))
                .count();
            // #404: state-binding requirements are neither consents nor secrets;
            // count them separately so the secret count stays accurate.
            let state_binding_count = requirements
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind,
                        InteractiveResolutionKind::StateBindingRequired { .. }
                    )
                })
                .count();
            let secret_count = requirements.len() - consent_count - state_binding_count;
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
            if state_binding_count > 0 {
                parts.push(format!("{state_binding_count} state folder(s) to choose"));
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
    use gpui::{ParentElement, Styled, rgb};

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
    use gpui::{ParentElement, Styled, rgb};

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
