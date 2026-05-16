pub mod command;
pub mod policy;
pub mod screenshot;
pub mod transport;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracing::warn;

use command::PendingAutomationRequest;
use transport::{ActivePaneSnapshot, NotifyFn, PendingQueue};

/// Thread-safe automation host. Owned by `WebViewManager`.
/// Clone is cheap — all shared state is behind `Arc`.
///
/// Intentionally contains no GPUI types (`AsyncApp`, `AnyWindowHandle`) so that
/// it is `Send + Sync` and can be captured by `evaluate_script_with_callback`
/// closures and socket-listener background threads.
/// GPUI wakeup is handled by `WebViewManager`, which polls `has_pending` from
/// a foreground timer task.
#[derive(Clone)]
pub struct AutomationHost {
    pub(crate) pending: PendingQueue,
    page_loaded_panes: Arc<Mutex<HashSet<usize>>>,
    /// Set to `true` by the socket listener or WaitFor retry when a new request
    /// is pushed. The GPUI foreground polling task reads and clears this flag.
    pub(crate) has_pending: Arc<AtomicBool>,
    /// Snapshot of the GPUI active pane id, updated by `WebViewManager` whenever
    /// the active pane changes. Read by the socket listener at request enqueue
    /// time so MCP `pane_id=0` resolves against the pane the caller saw, not the
    /// pane that happens to be active when the GPUI tick eventually picks the
    /// request up (#67).
    active_pane: ActivePaneSnapshot,
}

impl AutomationHost {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(Vec::new())),
            page_loaded_panes: Arc::new(Mutex::new(HashSet::new())),
            has_pending: Arc::new(AtomicBool::new(false)),
            active_pane: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the Unix socket listener. Returns the socket path on success.
    pub fn start(&self) -> Option<PathBuf> {
        let pending = Arc::clone(&self.pending);
        let has_pending = Arc::clone(&self.has_pending);
        let active_pane = Arc::clone(&self.active_pane);

        let notify: NotifyFn = Arc::new(move || {
            has_pending.store(true, Ordering::Relaxed);
        });

        match transport::start_socket_listener(pending, notify, active_pane) {
            Ok(path) => {
                tracing::info!(socket = %path.display(), "automation socket listening");
                Some(path)
            }
            Err(e) => {
                warn!("automation socket start failed: {e}");
                None
            }
        }
    }

    /// Update the active-pane snapshot used by the socket listener to resolve
    /// `pane_id=0` at request-enqueue time.
    pub fn set_active_pane(&self, pane_id: Option<usize>) {
        if let Ok(mut guard) = self.active_pane.lock() {
            *guard = pane_id;
        }
    }

    // ── Page-load lifecycle ────────────────────────────────────────────────

    pub fn mark_page_loaded(&self, pane_id: usize) {
        if let Ok(mut set) = self.page_loaded_panes.lock() {
            set.insert(pane_id);
        }
    }

    pub fn mark_page_unloaded(&self, pane_id: usize) {
        if let Ok(mut set) = self.page_loaded_panes.lock() {
            set.remove(&pane_id);
        }
    }

    pub fn is_page_loaded(&self, pane_id: usize) -> bool {
        self.page_loaded_panes
            .lock()
            .map(|set| set.contains(&pane_id))
            .unwrap_or(false)
    }

    // ── Request queue ──────────────────────────────────────────────────────

    /// Drain all pending requests. Called from the GPUI main thread in `sync_from_state`.
    pub fn drain_requests(&self) -> Vec<PendingAutomationRequest> {
        self.pending
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// Prepend requests back onto the queue (for WaitFor retries).
    pub fn requeue(&self, requests: Vec<PendingAutomationRequest>) {
        if requests.is_empty() {
            return;
        }
        if let Ok(mut q) = self.pending.lock() {
            let existing = std::mem::take(&mut *q);
            *q = requests;
            q.extend(existing);
        }
    }

    /// Returns a clone of this host (cheap — all fields are `Arc`).
    pub fn clone_host(&self) -> Self {
        self.clone()
    }

    /// Fail all pending requests targeting a given pane (e.g. when the pane is dropped).
    pub fn fail_requests_for_pane(&self, pane_id: usize) {
        let to_fail: Vec<PendingAutomationRequest> = {
            let mut q = match self.pending.lock() {
                Ok(q) => q,
                Err(_) => return,
            };
            let (failed, keep): (Vec<_>, Vec<_>) = q.drain(..).partition(|r| r.pane_id == pane_id);
            *q = keep;
            failed
        };
        for req in to_fail {
            req.send(Err("pane was destroyed".into()));
        }
    }
}

// Register AutomationHost as a GPUI global so dock.rs can clone
// it from the App context to set up the page-load handler.
impl gpui::Global for AutomationHost {}
