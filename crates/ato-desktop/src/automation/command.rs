use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::policy::MCP_IMPLICIT_PAGE_LOAD_TIMEOUT;

// ── JSON-RPC 2.0 wire types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ── Automation commands ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AutomationCommand {
    /// Returns the Playwright-compatible a11y snapshot of the page.
    Snapshot,
    /// Takes a PNG screenshot of the WebView. macOS only.
    Screenshot,
    /// Clicks element by stable ref.
    Click { ref_id: String },
    /// Fills an input by ref with a value (clears first, like fill).
    Fill { ref_id: String, value: String },
    /// Types text character-by-character into the focused/specified element.
    Type { ref_id: String, text: String },
    /// Selects an option in a `<select>` by value or text.
    SelectOption { ref_id: String, value: String },
    /// Sets checkbox/radio checked state.
    Check { ref_id: String, checked: bool },
    /// Presses a key on the currently focused element.
    PressKey { key: String },
    /// Navigates the WebView to a URL.
    Navigate { url: String },
    /// Goes back in the history stack.
    NavigateBack,
    /// Goes forward in the history stack.
    NavigateForward,
    /// Waits until selector matches. Immediate check; Rust retries on not-found.
    WaitFor { selector: String, timeout_ms: u64 },
    /// Evaluates arbitrary JS and returns the result.
    Evaluate { expression: String },
    /// Returns buffered console messages and clears the buffer.
    ConsoleMessages,
    /// Checks whether text appears in the page's text content.
    VerifyTextVisible { text: String },
    /// Checks whether the element referenced is visible.
    VerifyElementVisible { ref_id: String },
    /// Lists all open panes with their IDs and URLs.
    ListPanes,
    /// Focuses a pane by ID (makes it the active pane).
    FocusPane { pane_id: usize },
    /// Opens a URL via the app-level omnibar (navigate_to_url).
    /// Works even when no pane is currently active; creates a new pane.
    OpenUrl { url: String },
    /// Persist one or more secrets for `handle`, grant them to that
    /// capsule, and (optionally) dismiss any open `pending_config`
    /// modal targeting the same handle so the launch re-arms with
    /// the freshly stored secrets.
    ///
    /// Mirrors `ui::DesktopShell::save_pending_config` (the modal
    /// Save handler): values flow through `AppState::add_secret` +
    /// `AppState::grant_secret_to_capsule`, both of which surface
    /// `SaveSecretsError` (#88) so a disk-write failure doesn't get
    /// swallowed.
    SetCapsuleSecrets {
        handle: String,
        secrets: Vec<(String, String)>,
        clear_pending_config: bool,
    },
}

// ── Pending request queue entry ──────────────────────────────────────────────

pub type ResponseSender = Arc<Mutex<Option<Sender<Result<Value, String>>>>>;

pub struct PendingAutomationRequest {
    /// Target pane. 0 = active pane (resolved at dispatch time).
    pub pane_id: usize,
    pub command: AutomationCommand,
    /// Shared sender so both the callback path and the error path can send.
    pub response_tx: ResponseSender,
    /// Used to enforce per-command timeouts (30 s default, configurable for WaitFor).
    pub created_at: Instant,
    /// For WaitFor: keep retrying until this deadline.
    pub wait_deadline: Option<Instant>,
}

impl PendingAutomationRequest {
    pub fn new(
        pane_id: usize,
        command: AutomationCommand,
        tx: Sender<Result<Value, String>>,
    ) -> Self {
        // WaitFor carries an explicit caller-controlled deadline; everything
        // else gets the implicit page-load grace so a fast `navigate -> click`
        // from MCP doesn't race the page load (#67).
        let wait_deadline = match &command {
            AutomationCommand::WaitFor { timeout_ms, .. } => {
                Some(Instant::now() + std::time::Duration::from_millis(*timeout_ms))
            }
            _ => Some(Instant::now() + MCP_IMPLICIT_PAGE_LOAD_TIMEOUT),
        };
        Self {
            pane_id,
            command,
            response_tx: Arc::new(Mutex::new(Some(tx))),
            created_at: Instant::now(),
            wait_deadline,
        }
    }

    /// Send a response through the shared channel. Returns false if already consumed.
    pub fn send(&self, result: Result<Value, String>) -> bool {
        if let Ok(mut guard) = self.response_tx.lock() {
            if let Some(tx) = guard.take() {
                return tx.send(result).is_ok();
            }
        }
        false
    }

    /// Clone the sender for use inside an async callback closure.
    pub fn clone_tx(&self) -> ResponseSender {
        Arc::clone(&self.response_tx)
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed().as_secs() > 30
    }
}
