//! Guard that marks the main thread as "inside WebView2 initialization".
//!
//! # Background
//!
//! On Windows, Wry's `build_as_child` calls `webview2_com::wait_with_pump`
//! (for both `create_environment` and `create_controller`), which pumps the
//! Windows message queue via `GetMessageA` + `DispatchMessageA` in a loop
//! while waiting for the WebView2 COM callbacks to fire.
//!
//! GPUI posts `WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD` when it schedules
//! foreground tasks. If `build_as_child` is called while an outer GPUI
//! `AsyncApp::update()` (i.e. `AppCell::borrow_mut()`) is held, any GPUI
//! foreground task that runs during the message pump will attempt a second
//! `borrow_mut()` and cause:
//!
//! ```text
//! thread 'main' panicked at async_context.rs:65:27: RefCell already borrowed
//! STATUS_STACK_BUFFER_OVERRUN
//! ```
//!
//! # Fix
//!
//! All `build_as_child` call sites MUST hold a [`WebviewInitGuard`] for the
//! duration of the call:
//!
//! ```rust
//! let _guard = crate::webview_init_guard::WebviewInitGuard::new();
//! let webview = builder.build_as_child(window).expect("...");
//! ```
//!
//! All periodic foreground drain loops MUST check [`WebviewInitGuard::is_active`]
//! at the top of each iteration (after the timer `.await`) and `continue` if it
//! returns `true`. This ensures the drain loop skips the current tick rather
//! than trying to re-acquire `borrow_mut()` while it is already held.
//!
//! Because every operation here is on the main thread, a `thread_local!`
//! `Cell<bool>` is sufficient — no atomics needed.

use std::cell::Cell;

thread_local! {
    static IN_PROGRESS: Cell<u32> = const { Cell::new(0) };
}

/// RAII guard that marks the main thread as being inside `build_as_child`.
///
/// Created with [`WebviewInitGuard::new`]; cleared on [`Drop`].
/// Drain loops check [`WebviewInitGuard::is_active`] and skip the
/// current iteration while this guard is held. Uses a depth counter so
/// that (defensively) nested guards on the same thread compose correctly:
/// the flag only clears once the outermost guard is dropped.
pub struct WebviewInitGuard;

impl WebviewInitGuard {
    /// Acquire the guard.  Must only be called on the GPUI main thread.
    #[inline]
    pub fn new() -> Self {
        IN_PROGRESS.with(|f| f.set(f.get() + 1));
        WebviewInitGuard
    }

    /// Returns `true` while any `WebviewInitGuard` is live on this thread.
    #[inline]
    pub fn is_active() -> bool {
        IN_PROGRESS.with(|f| f.get() > 0)
    }
}

impl Default for WebviewInitGuard {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WebviewInitGuard {
    #[inline]
    fn drop(&mut self) {
        IN_PROGRESS.with(|f| f.set(f.get().saturating_sub(1)));
    }
}

/// Yield (re-arming a short background timer) until no `WebviewInitGuard`
/// is active on the main thread.
///
/// Foreground tasks that resume from an `.await` and then call
/// `AsyncApp::update` MUST `await` this first when there is any chance they
/// resume during another window's `build_as_child`: otherwise the resumed
/// poll runs *inside* Wry's pumped WebView2 init while an outer `App` borrow
/// is held, and the `update` double-borrows (`RefCell already borrowed`).
///
/// Periodic drain loops can instead use the cheaper
/// `if WebviewInitGuard::is_active() { continue; }` pattern at the top of
/// each iteration; this helper is for one-shot continuations that have no
/// natural "skip this tick" loop to fall back to.
pub async fn wait_until_idle(background: &gpui::BackgroundExecutor) {
    use std::time::Duration;
    while WebviewInitGuard::is_active() {
        background.timer(Duration::from_millis(2)).await;
    }
}
