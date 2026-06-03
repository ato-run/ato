//! Session-owned capsule lifecycle registry.
//!
//! `SessionRegistry` is the single source of truth for capsule process state.
//! It lives on `AppState` and tracks every capsule session started by Desktop,
//! regardless of whether the session has an Ato window, an OS browser client,
//! or no visible surface at all (headless).
//!
//! Window/pane display is modelled as `SessionClient` attachments — a single
//! session can have zero or more clients.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::time::SystemTime;

use crate::orchestrator::GuestLaunchSession;

// ── Client identity ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionClientId(u64);

impl SessionClientId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LaunchRequestId(u64);

impl LaunchRequestId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

// ── Session (process lifecycle owner) ───────────────────────────────────────

/// Where the session was launched from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchVia {
    /// Started by the Desktop shell (Consent wizard, Focus View, etc.)
    Desktop,
    /// Started manually via the CLI (`ato app session start`)
    Cli,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciImportKind {
    Compose,
    DockerRunScript,
    ExplicitOci,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OciSessionStatus {
    Running,
    Stopped,
    StopFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesktopSessionKind {
    NativeSource,
    Oci {
        import_kind: OciImportKind,
        status: OciSessionStatus,
        endpoint_url: Option<String>,
        service_count: usize,
        source_path: Option<String>,
        source_hash: Option<String>,
    },
}

impl DesktopSessionKind {
    fn is_oci(&self) -> bool {
        matches!(self, Self::Oci { .. })
    }
}

/// Safe OCI session fields read from the CLI `ato ps --json` boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciSessionSnapshot {
    pub id: String,
    pub import_kind: OciImportKind,
    pub status: OciSessionStatus,
    pub endpoint_url: Option<String>,
    pub service_count: usize,
    pub source_path: Option<String>,
    pub source_hash: Option<String>,
}

/// The canonical lifecycle state of a capsule *process*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionProcessState {
    /// Launch is in progress (CLI is resolving / starting the capsule).
    Starting,
    /// Process is running and health-checkable.
    Ready,
    /// `stop_guest_session` has been called; waiting for the CLI to confirm.
    Stopping,
    /// The session has been stopped successfully.
    Stopped,
    /// `stop_guest_session` returned an error; the process may still be alive.
    FailedToStop { error: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopSessionRequest {
    pub session_id: String,
    pub is_oci: bool,
}

/// All information needed to re-launch or restart a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleLaunchContext {
    pub handle_or_url: String,
    pub target: Option<String>,
    pub launch_configs: Vec<(String, String)>,
    pub requested_client: SessionClientKind,
    pub source: CapsuleOpenSource,
}

/// Who initiated the open request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleOpenSource {
    NavigateToUrl,
    Dock,
    StartPage,
    CardSwitcher,
    Automation,
}

/// The session record — owns the process lifecycle.
#[derive(Clone, Debug)]
pub struct CapsuleSession {
    pub session_id: String,
    pub handle: String,
    pub canonical_handle: Option<String>,
    pub title: String,
    pub process_state: SessionProcessState,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub session_kind: DesktopSessionKind,
    pub launch_context: CapsuleLaunchContext,
    pub launch_via: LaunchVia,
    pub created_at: SystemTime,
    pub last_seen_at: SystemTime,
}

impl CapsuleSession {
    pub fn from_launch_session(
        session: &GuestLaunchSession,
        launch_context: CapsuleLaunchContext,
    ) -> Self {
        Self {
            session_id: session.session_id.clone(),
            handle: session.handle.clone(),
            canonical_handle: session.canonical_handle.clone(),
            title: session
                .snapshot_label
                .clone()
                .unwrap_or_else(|| session.handle.clone()),
            process_state: SessionProcessState::Ready,
            local_url: session.local_url.clone(),
            healthcheck_url: session.healthcheck_url.clone(),
            session_kind: DesktopSessionKind::NativeSource,
            launch_context,
            launch_via: LaunchVia::Desktop,
            created_at: SystemTime::now(),
            last_seen_at: SystemTime::now(),
        }
    }

    pub fn from_oci_snapshot(snapshot: OciSessionSnapshot) -> Self {
        let source_label = snapshot
            .source_path
            .clone()
            .unwrap_or_else(|| snapshot.id.clone());
        let process_state = match &snapshot.status {
            OciSessionStatus::Running => SessionProcessState::Ready,
            OciSessionStatus::Stopped => SessionProcessState::Stopped,
            OciSessionStatus::StopFailed => SessionProcessState::FailedToStop {
                error: "OCI cleanup needs retry".to_string(),
            },
        };
        Self {
            session_id: snapshot.id.clone(),
            handle: source_label.clone(),
            canonical_handle: None,
            title: source_label.clone(),
            process_state,
            local_url: snapshot.endpoint_url.clone(),
            healthcheck_url: None,
            session_kind: DesktopSessionKind::Oci {
                import_kind: snapshot.import_kind,
                status: snapshot.status,
                endpoint_url: snapshot.endpoint_url,
                service_count: snapshot.service_count,
                source_path: snapshot.source_path,
                source_hash: snapshot.source_hash,
            },
            launch_context: CapsuleLaunchContext {
                handle_or_url: source_label,
                target: None,
                launch_configs: Vec::new(),
                requested_client: SessionClientKind::Headless,
                source: CapsuleOpenSource::CardSwitcher,
            },
            launch_via: LaunchVia::Cli,
            created_at: SystemTime::now(),
            last_seen_at: SystemTime::now(),
        }
    }
}

// ── Client (display surface) ─────────────────────────────────────────────────

/// What kind of display surface is attached to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum SessionClientKind {
    /// Focus View top-level GPUI window.
    AtoWindow,
    /// Legacy single-window pane inside DesktopShell.
    WebViewPane,
    /// Opened in the user's OS default browser (no Ato pane).
    OsBrowser,
    /// No visible surface; tracked purely for supervision.
    Headless,
}

/// Per-client lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum SessionClientState {
    /// Window/Pane is open and actively displaying the capsule.
    Attached,
    /// Window was closed but the session process is still running.
    Detached,
    /// Displayed externally (OS browser); no Ato window/pane exists.
    External,
    /// Window/Pane is in the process of closing.
    Closing,
}

/// A display attachment for a session. A session can have zero or more clients.
#[derive(Clone, Debug)]
pub struct SessionClient {
    pub client_id: SessionClientId,
    pub session_id: String,
    pub client_kind: SessionClientKind,
    pub window_id: Option<u64>,
    pub pane_id: Option<usize>,
    pub state: SessionClientState,
    pub attached_at: SystemTime,
    pub last_seen_at: SystemTime,
}

// ── Open Windows view model ─────────────────────────────────────────────────

/// Derived presentation state for the Open Windows / Card Switcher UI.
/// Determined by priority: Failed/Stopped > Visible > External > Detached > Headless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum PresentationState {
    Failed,
    Stopped,
    Visible,
    External,
    Detached,
    Headless,
}

impl PresentationState {
    /// Lower value = higher display priority.
    pub fn priority(self) -> u8 {
        match self {
            Self::Failed => 0,
            Self::Stopped => 1,
            Self::Visible => 2,
            Self::External => 3,
            Self::Detached => 4,
            Self::Headless => 5,
        }
    }
}

/// Compact client info attached to a `SessionViewEntry`.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ClientSummary {
    pub client_kind: SessionClientKind,
    pub state: SessionClientState,
}

/// One row in the Open Windows screen.  Always 1 session = 1 row.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SessionViewEntry {
    pub session_id: String,
    pub title: String,
    pub handle: String,
    pub presentation_state: PresentationState,
    pub attached_clients: Vec<ClientSummary>,
    pub primary_window_id: Option<u64>,
    pub local_url: Option<String>,
    pub session_kind: DesktopSessionKind,
}

// ── SessionRegistry ─────────────────────────────────────────────────────────

/// Single source of truth for all capsule sessions managed by Desktop.
///
/// Lives on `AppState.sessions`.  Tracks sessions, their display clients,
/// and a `window_id → client_id[]` mapping for window-close handling.
#[derive(Clone, Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, CapsuleSession>,
    clients: HashMap<SessionClientId, SessionClient>,
    window_to_clients: HashMap<u64, Vec<SessionClientId>>,
    next_client_id: u64,
}

impl SessionRegistry {
    // ── session operations ───────────────────────────────────────────────

    /// Register a newly-launched session.
    pub fn register_session(&mut self, session: CapsuleSession) {
        self.sessions.insert(session.session_id.clone(), session);
    }

    /// Remove a session and all its clients.  Does NOT stop the process —
    /// callers must stop the session before removing if desired.
    pub fn remove_session(&mut self, session_id: &str) {
        // Remove all clients for this session.
        let client_ids: Vec<SessionClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session_id == session_id)
            .map(|(id, _)| *id)
            .collect();
        for cid in &client_ids {
            if let Some(client) = self.clients.remove(cid)
                && let Some(wid) = client.window_id
            {
                self.window_to_clients.remove(&wid);
            }
        }
        self.sessions.remove(session_id);
    }

    /// Get an immutable reference to a session.
    pub fn get_session(&self, session_id: &str) -> Option<&CapsuleSession> {
        self.sessions.get(session_id)
    }

    /// Update the process state of a session.
    pub fn update_process_state(&mut self, session_id: &str, state: SessionProcessState) {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.process_state = state;
            session.last_seen_at = SystemTime::now();
        }
    }

    // ── client operations ────────────────────────────────────────────────

    /// Attach a new display client to a session.
    pub fn attach_client(&mut self, client: SessionClient) -> SessionClientId {
        let cid = client.client_id;
        if let Some(window_id) = client.window_id {
            self.window_to_clients
                .entry(window_id)
                .or_default()
                .push(cid);
        }
        self.clients.insert(cid, client);
        cid
    }

    /// Detach a client (mark it as no longer displaying the capsule).
    /// Does not stop the session process.
    pub fn detach_client(&mut self, client_id: SessionClientId) {
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.state = SessionClientState::Detached;
            client.last_seen_at = SystemTime::now();
            // Remove from window mapping so this window-id is no longer
            // considered "attached".
            if let Some(wid) = client.window_id
                && let Some(ids) = self.window_to_clients.get_mut(&wid)
            {
                ids.retain(|id| *id != client_id);
                if ids.is_empty() {
                    self.window_to_clients.remove(&wid);
                }
            }
        }
    }

    // ── queries ──────────────────────────────────────────────────────────

    /// Return all client IDs attached to a given GPUI window.
    pub fn clients_by_window_id(&self, window_id: u64) -> Vec<SessionClientId> {
        self.window_to_clients
            .get(&window_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get the session ID for a given client.
    pub fn session_id_for_client(&self, client_id: SessionClientId) -> Option<&str> {
        self.clients.get(&client_id).map(|c| c.session_id.as_str())
    }

    /// Return all clients attached to a given session.
    pub fn clients_for_session(&self, session_id: &str) -> Vec<&SessionClient> {
        self.clients
            .values()
            .filter(|c| c.session_id == session_id)
            .collect()
    }

    /// Replace the CLI-originated OCI projection without touching Desktop-owned
    /// source sessions or their attached window clients.
    pub fn sync_oci_sessions(&mut self, snapshots: Vec<OciSessionSnapshot>) {
        let incoming_ids: Vec<String> =
            snapshots.iter().map(|session| session.id.clone()).collect();
        let stale_ids: Vec<String> = self
            .sessions
            .values()
            .filter(|session| {
                session.session_kind.is_oci() && !incoming_ids.contains(&session.session_id)
            })
            .map(|session| session.session_id.clone())
            .collect();
        let removed = stale_ids.len();
        for id in stale_ids {
            self.remove_session(&id);
        }
        let registered = snapshots.len();
        for snapshot in snapshots {
            // `provider` (podman|docker) is not carried on `OciSessionSnapshot`
            // yet — `ato ps` reports it on the host-machine entry, not per app
            // session. Log `unknown` rather than guessing a provider, so the
            // Docker/Podman comparison the issue asks for is not misled.
            tracing::info!(
                session_id = %snapshot.id,
                runtime_kind = "oci",
                provider = "unknown",
                service_count = snapshot.service_count,
                status = ?snapshot.status,
                "app instance registered in desktop state (oci)"
            );
            self.register_session(CapsuleSession::from_oci_snapshot(snapshot));
        }
        tracing::info!(
            registered,
            removed,
            "sync_oci_sessions: desktop OCI projection synced"
        );
    }

    // ── lifecycle actions ──────────────────────────────────────────────

    /// Detach all clients associated with a GPUI window, without stopping
    /// the session processes.
    pub fn detach_clients_by_window_id(&mut self, window_id: u64) -> Vec<String> {
        let client_ids = self.clients_by_window_id(window_id);
        let mut session_ids = Vec::new();
        for cid in &client_ids {
            if let Some(sid) = self.session_id_for_client(*cid) {
                session_ids.push(sid.to_string());
            }
            self.detach_client(*cid);
        }
        session_ids.sort();
        session_ids.dedup();
        session_ids
    }

    /// Mark a session as `Stopping` and return the stop work that must run
    /// outside the UI thread. Only acts once — if the session is already
    /// `Stopping` or `Stopped`, this is a no-op.
    pub fn begin_stop_session_once(&mut self, session_id: &str) -> Option<StopSessionRequest> {
        let needs_stop = matches!(
            self.sessions.get(session_id),
            Some(s)
                if !matches!(
                    s.process_state,
                    SessionProcessState::Stopping | SessionProcessState::Stopped
                )
        );
        if !needs_stop {
            return None;
        }
        self.update_process_state(session_id, SessionProcessState::Stopping);

        let is_oci = self
            .sessions
            .get(session_id)
            .map(|session| session.session_kind.is_oci())
            .unwrap_or(false);
        Some(StopSessionRequest {
            session_id: session_id.to_string(),
            is_oci,
        })
    }

    /// Backwards-compatible fire-and-forget stop path. UI callers should use
    /// `begin_stop_session_once` plus a completion update so presentation state
    /// does not remain stuck at `Stopping`.
    pub fn stop_session_once(&mut self, session_id: &str) {
        let Some(request) = self.begin_stop_session_once(session_id) else {
            return;
        };
        let sid = request.session_id;
        let is_oci = request.is_oci;
        std::thread::spawn(move || {
            let stop_result = if is_oci {
                crate::orchestrator::stop_oci_session(&sid).map(|()| true)
            } else {
                crate::orchestrator::stop_guest_session(&sid)
            };
            match stop_result {
                Ok(true) => {
                    tracing::info!(session_id = %sid, "stop_session_once: stopped");
                }
                Ok(false) => {
                    tracing::info!(session_id = %sid, "stop_session_once: already inactive");
                }
                Err(err) => {
                    tracing::error!(session_id = %sid, error = %err, "stop_session_once: stop failed");
                }
            }
            // TODO(D4): post completion to UI thread to update process_state to Stopped
        });
    }

    pub fn finish_stop_session(&mut self, session_id: &str, result: Result<bool, String>) {
        match result {
            Ok(true) | Ok(false) => {
                self.update_process_state(session_id, SessionProcessState::Stopped);
            }
            Err(error) => {
                self.update_process_state(session_id, SessionProcessState::FailedToStop { error });
            }
        }
    }

    /// Mark every running session (Starting or Ready) as `Stopping` and return
    /// the stop work, without spawning any background threads. Unlike
    /// [`Self::stop_all_running`] (fire-and-forget), this hands completion to
    /// the caller, which must run each stop and call [`Self::finish_stop_session`]
    /// with the result so presentation state does not stay stuck at `Stopping`.
    ///
    /// Used by the Windows tray's `Stop All` / `Quit`, where the caller needs to
    /// observe completion (to update running state, or to wait before quitting).
    pub fn begin_stop_all(&mut self) -> Vec<StopSessionRequest> {
        let running: Vec<String> = self
            .sessions
            .values()
            .filter(|s| {
                matches!(
                    s.process_state,
                    SessionProcessState::Starting | SessionProcessState::Ready
                )
            })
            .map(|s| s.session_id.clone())
            .collect();
        running
            .iter()
            .filter_map(|sid| self.begin_stop_session_once(sid))
            .collect()
    }

    /// Stop every session that is still running (Starting or Ready).
    /// Called on app quit so the Focus-mode close path (which lacks
    /// `WebViewManager::Drop`) does not leave orphan processes.
    ///
    /// Delegates to `stop_session_once` which guards against double-stop.
    pub fn stop_all_running(&mut self) -> usize {
        let running: Vec<String> = self
            .sessions
            .values()
            .filter(|s| {
                matches!(
                    s.process_state,
                    SessionProcessState::Starting | SessionProcessState::Ready
                )
            })
            .map(|s| s.session_id.clone())
            .collect();
        let count = running.len();
        for sid in &running {
            self.stop_session_once(sid);
        }
        count
    }

    // ── view model ───────────────────────────────────────────────────────

    /// Build `SessionViewEntry` rows for the Open Windows screen.
    /// Always returns 1 row per session.
    pub fn view_entries(&self) -> Vec<SessionViewEntry> {
        let mut entries: Vec<SessionViewEntry> = self
            .sessions
            .values()
            .map(|session| {
                let clients = self.clients_for_session(&session.session_id);
                let summaries: Vec<ClientSummary> = clients
                    .iter()
                    .map(|c| ClientSummary {
                        client_kind: c.client_kind,
                        state: c.state,
                    })
                    .collect();

                let presentation_state =
                    Self::derive_presentation_state(&session.process_state, &summaries);

                let primary_window_id = clients
                    .iter()
                    .find(|c| matches!(c.state, SessionClientState::Attached))
                    .and_then(|c| c.window_id);

                SessionViewEntry {
                    session_id: session.session_id.clone(),
                    title: session.title.clone(),
                    handle: session.handle.clone(),
                    presentation_state,
                    attached_clients: summaries,
                    primary_window_id,
                    local_url: session.local_url.clone(),
                    session_kind: session.session_kind.clone(),
                }
            })
            .collect();

        // MRU order: most recently seen first.
        entries.sort_by_key(|e| {
            self.sessions
                .get(&e.session_id)
                .map(|s| s.last_seen_at)
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
        entries.reverse();
        entries
    }

    /// Build the Dock / background-app rows. Foreground WebView apps are
    /// represented by `OpenContentWindows` cards with screenshots, so keeping
    /// them here would make visual apps look like generic "Running Apps".
    pub fn background_view_entries(&self) -> Vec<SessionViewEntry> {
        self.view_entries()
            .into_iter()
            .filter(|entry| entry.presentation_state != PresentationState::Visible)
            .collect()
    }

    /// Derive the display presentation state from process state and client summaries.
    /// Priority: Failed/Stopped > Visible > External > Detached > Headless.
    fn derive_presentation_state(
        process_state: &SessionProcessState,
        clients: &[ClientSummary],
    ) -> PresentationState {
        // Terminal states always win.
        match process_state {
            SessionProcessState::FailedToStop { .. } => return PresentationState::Failed,
            SessionProcessState::Stopped => return PresentationState::Stopped,
            _ => {}
        }

        // Best client state determines the derived presentation.
        let mut best: Option<PresentationState> = None;
        for summary in clients {
            let candidate = match (summary.client_kind, summary.state) {
                (_, SessionClientState::Attached) => PresentationState::Visible,
                (SessionClientKind::OsBrowser, SessionClientState::External) => {
                    PresentationState::External
                }
                (SessionClientKind::Headless, _) => PresentationState::Headless,
                (_, SessionClientState::Detached) => PresentationState::Detached,
                (_, SessionClientState::Closing) => PresentationState::Detached, // treat as detaching
                (_, SessionClientState::External) => PresentationState::External,
            };

            match best {
                None => best = Some(candidate),
                Some(current) if candidate.priority() < current.priority() => {
                    best = Some(candidate);
                }
                _ => {}
            }
        }

        best.unwrap_or(PresentationState::Headless)
    }
}

// ── Pending launches ────────────────────────────────────────────────────────

/// A launch request that may be awaiting user approval (E103/E302 modal).
#[derive(Clone, Debug)]
pub struct CapsuleLaunchRequest {
    pub launch_id: LaunchRequestId,
    pub handle_or_url: String,
    pub target: Option<String>,
    pub requested_client: SessionClientKind,
    pub source: CapsuleOpenSource,
    pub origin_window_id: Option<u64>,
    pub created_at: SystemTime,
}

/// Tracks the lifecycle of a pending launch request through the consent flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingLaunchState {
    /// User has not yet submitted the consent/secrets form.
    AwaitingApproval,
    /// User approved; `prepare_launch_session` is running.
    ApprovedStarting,
    /// `prepare_launch_session` returned E103/E302 again — re-blocked.
    BlockedAgain,
}

/// Manages all in-flight launch requests that may be blocked on user approval.
/// Uses a `launch_id → (request, state)` map so multiple simultaneous launches
/// do not overwrite each other.
#[derive(Clone, Debug, Default)]
pub struct PendingLaunches {
    pub launches: HashMap<LaunchRequestId, (CapsuleLaunchRequest, PendingLaunchState)>,
}

impl PendingLaunches {
    pub fn insert(&mut self, request: CapsuleLaunchRequest) {
        self.launches.insert(
            request.launch_id,
            (request, PendingLaunchState::AwaitingApproval),
        );
    }

    pub fn get(&self, launch_id: LaunchRequestId) -> Option<&CapsuleLaunchRequest> {
        self.launches.get(&launch_id).map(|(r, _)| r)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, handle: &str) -> CapsuleSession {
        CapsuleSession {
            session_id: id.to_string(),
            handle: handle.to_string(),
            canonical_handle: None,
            title: handle.to_string(),
            process_state: SessionProcessState::Ready,
            local_url: Some(format!("http://127.0.0.1:8080/{}", id)),
            healthcheck_url: None,
            session_kind: DesktopSessionKind::NativeSource,
            launch_context: CapsuleLaunchContext {
                handle_or_url: handle.to_string(),
                target: None,
                launch_configs: vec![],
                requested_client: SessionClientKind::AtoWindow,
                source: CapsuleOpenSource::NavigateToUrl,
            },
            launch_via: LaunchVia::Desktop,
            created_at: SystemTime::now(),
            last_seen_at: SystemTime::now(),
        }
    }

    fn make_client(
        id: SessionClientId,
        session_id: &str,
        kind: SessionClientKind,
        window_id: Option<u64>,
        state: SessionClientState,
    ) -> SessionClient {
        SessionClient {
            client_id: id,
            session_id: session_id.to_string(),
            client_kind: kind,
            window_id,
            pane_id: None,
            state,
            attached_at: SystemTime::now(),
            last_seen_at: SystemTime::now(),
        }
    }

    #[test]
    fn register_session_and_retrieve() {
        let mut reg = SessionRegistry::default();
        let s = make_session("s1", "test/capsule");
        reg.register_session(s);
        assert!(reg.get_session("s1").is_some());
        assert_eq!(reg.get_session("s1").unwrap().handle, "test/capsule");
    }

    #[test]
    fn attach_two_clients_to_same_session() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));

        let c1 = make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        );
        let c2 = make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::OsBrowser,
            None,
            SessionClientState::External,
        );

        reg.attach_client(c1);
        reg.attach_client(c2);

        let clients = reg.clients_for_session("s1");
        assert_eq!(clients.len(), 2);
    }

    #[test]
    fn detach_client_keeps_session_alive() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        let cid = SessionClientId::next();
        reg.attach_client(make_client(
            cid,
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        reg.detach_client(cid);
        assert!(reg.get_session("s1").is_some());
        let clients = reg.clients_for_session("s1");
        assert_eq!(clients[0].state, SessionClientState::Detached);
    }

    #[test]
    fn remove_session_removes_all_clients() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::OsBrowser,
            None,
            SessionClientState::External,
        ));

        reg.remove_session("s1");
        assert!(reg.get_session("s1").is_none());
        assert_eq!(reg.clients_for_session("s1").len(), 0);
    }

    #[test]
    fn clients_by_window_id_returns_correct_clients() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "c1"));
        reg.register_session(make_session("s2", "c2"));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s2",
            SessionClientKind::AtoWindow,
            Some(200),
            SessionClientState::Attached,
        ));

        assert_eq!(reg.clients_by_window_id(100).len(), 1);
        assert_eq!(reg.clients_by_window_id(200).len(), 1);
        assert_eq!(reg.clients_by_window_id(999).len(), 0);
    }

    #[test]
    fn session_id_for_client_returns_correct_id() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "c1"));
        let cid = SessionClientId::next();
        reg.attach_client(make_client(
            cid,
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        assert_eq!(reg.session_id_for_client(cid), Some("s1"));
    }

    #[test]
    fn view_entries_one_session_one_row() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test"));
        // Two clients — still one row.
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::OsBrowser,
            None,
            SessionClientState::External,
        ));

        let entries = reg.view_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attached_clients.len(), 2);
    }

    #[test]
    fn view_entries_visible_wins_over_external() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test"));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::OsBrowser,
            None,
            SessionClientState::External,
        ));

        let entries = reg.view_entries();
        assert_eq!(entries[0].presentation_state, PresentationState::Visible);
    }

    #[test]
    fn background_view_entries_excludes_visible_sessions() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("visible", "foreground"));
        reg.register_session(make_session("headless", "background"));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "visible",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        let entries = reg.background_view_entries();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "headless");
        assert_eq!(entries[0].presentation_state, PresentationState::Headless);
    }

    #[test]
    fn view_entries_stopped_wins_over_visible() {
        let mut reg = SessionRegistry::default();
        let mut s = make_session("s1", "test");
        s.process_state = SessionProcessState::Stopped;
        reg.register_session(s);
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        let entries = reg.view_entries();
        assert_eq!(entries[0].presentation_state, PresentationState::Stopped);
    }

    #[test]
    fn process_state_transitions() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test"));
        assert!(matches!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Ready
        ));

        reg.update_process_state("s1", SessionProcessState::Stopping);
        assert!(matches!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopping
        ));

        reg.update_process_state("s1", SessionProcessState::Stopped);
        assert!(matches!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopped
        ));
    }

    #[test]
    fn pending_launches_does_not_overwrite() {
        let mut pl = PendingLaunches::default();
        let req1 = CapsuleLaunchRequest {
            launch_id: LaunchRequestId::next(),
            handle_or_url: "c1".into(),
            target: None,
            requested_client: SessionClientKind::AtoWindow,
            source: CapsuleOpenSource::NavigateToUrl,
            origin_window_id: None,
            created_at: SystemTime::now(),
        };
        let req2 = CapsuleLaunchRequest {
            launch_id: LaunchRequestId::next(),
            handle_or_url: "c2".into(),
            target: None,
            requested_client: SessionClientKind::OsBrowser,
            source: CapsuleOpenSource::Dock,
            origin_window_id: None,
            created_at: SystemTime::now(),
        };
        pl.insert(req1.clone());
        pl.insert(req2.clone());
        assert_eq!(pl.launches.len(), 2);
        assert!(pl.get(req1.launch_id).is_some());
        assert!(pl.get(req2.launch_id).is_some());
    }

    #[test]
    fn detach_all_clients_keeps_session_alive() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        let cid = SessionClientId::next();
        reg.attach_client(make_client(
            cid,
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        let affected = reg.detach_clients_by_window_id(100);
        assert_eq!(affected, vec!["s1"]);
        assert!(reg.get_session("s1").is_some());
        let clients = reg.clients_for_session("s1");
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].state, SessionClientState::Detached);
    }

    #[test]
    fn detach_and_stop_removes_session_and_process() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        let cid = SessionClientId::next();
        reg.attach_client(make_client(
            cid,
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));

        reg.detach_clients_by_window_id(100);
        reg.update_process_state("s1", SessionProcessState::Stopping);
        reg.update_process_state("s1", SessionProcessState::Stopped);

        assert!(reg.get_session("s1").is_some());
        assert_eq!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopped
        );
    }

    #[test]
    fn stop_on_already_stopped_session_is_noop() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));

        reg.update_process_state("s1", SessionProcessState::Stopped);
        let state_before = reg.get_session("s1").unwrap().process_state.clone();
        let request = reg.begin_stop_session_once("s1");
        let state_after = reg.get_session("s1").unwrap().process_state.clone();
        assert!(request.is_none());
        assert_eq!(state_before, state_after);
        assert_eq!(state_after, SessionProcessState::Stopped);
    }

    #[test]
    fn begin_stop_session_once_marks_stopping_and_returns_request() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));

        let request = reg.begin_stop_session_once("s1").expect("stop request");

        assert_eq!(request.session_id, "s1");
        assert!(!request.is_oci);
        assert_eq!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopping
        );
        assert!(reg.begin_stop_session_once("s1").is_none());
    }

    #[test]
    fn finish_stop_session_updates_terminal_state() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        reg.begin_stop_session_once("s1").expect("stop request");

        reg.finish_stop_session("s1", Ok(true));

        assert_eq!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopped
        );

        reg.register_session(make_session("s2", "test/capsule-2"));
        reg.begin_stop_session_once("s2").expect("stop request");
        reg.finish_stop_session("s2", Err("timeout".to_string()));

        assert!(matches!(
            &reg.get_session("s2").unwrap().process_state,
            SessionProcessState::FailedToStop { error } if error == "timeout"
        ));
    }

    #[test]
    fn detach_clients_by_unknown_window_returns_empty() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        let affected = reg.detach_clients_by_window_id(999);
        assert!(affected.is_empty());
        assert!(reg.get_session("s1").is_some());
    }

    #[test]
    fn detach_keeps_other_window_clients() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "test/capsule"));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(100),
            SessionClientState::Attached,
        ));
        reg.attach_client(make_client(
            SessionClientId::next(),
            "s1",
            SessionClientKind::AtoWindow,
            Some(200),
            SessionClientState::Attached,
        ));

        let affected = reg.detach_clients_by_window_id(100);
        assert_eq!(affected, vec!["s1"]);
        let remaining: Vec<_> = reg
            .clients_for_session("s1")
            .into_iter()
            .filter(|c| c.state == SessionClientState::Attached)
            .collect();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].window_id, Some(200));
    }

    #[test]
    fn stop_all_running_stops_only_running_sessions() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "t1"));
        reg.register_session(make_session("s2", "t2"));
        reg.register_session(make_session("s3", "t3"));

        reg.update_process_state("s1", SessionProcessState::Ready);
        reg.update_process_state("s2", SessionProcessState::Stopped);
        reg.update_process_state("s3", SessionProcessState::Stopping);

        let count = reg.stop_all_running();
        assert_eq!(count, 1);
        assert_eq!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopping
        );
        assert_eq!(
            reg.get_session("s2").unwrap().process_state,
            SessionProcessState::Stopped
        );
    }

    #[test]
    fn begin_stop_all_marks_running_stopping_and_returns_requests() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "t1"));
        reg.register_session(make_session("s2", "t2"));
        reg.register_session(make_session("s3", "t3"));
        reg.update_process_state("s2", SessionProcessState::Stopped);
        reg.update_process_state("s3", SessionProcessState::Stopping);

        let requests = reg.begin_stop_all();

        // Only the Ready session yields a request; it is now Stopping.
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].session_id, "s1");
        assert_eq!(
            reg.get_session("s1").unwrap().process_state,
            SessionProcessState::Stopping
        );
        // Already-terminal/stopping sessions are untouched and not re-requested.
        assert_eq!(
            reg.get_session("s2").unwrap().process_state,
            SessionProcessState::Stopped
        );
        // A second call is a no-op now that nothing is running.
        assert!(reg.begin_stop_all().is_empty());
    }

    #[test]
    fn stop_all_running_on_all_stopped_returns_zero() {
        let mut reg = SessionRegistry::default();
        reg.register_session(make_session("s1", "t1"));
        reg.update_process_state("s1", SessionProcessState::Stopped);

        let count = reg.stop_all_running();
        assert_eq!(count, 0);
    }

    fn make_oci_snapshot(id: &str, status: OciSessionStatus) -> OciSessionSnapshot {
        OciSessionSnapshot {
            id: id.to_string(),
            import_kind: OciImportKind::DockerRunScript,
            status,
            endpoint_url: Some("http://127.0.0.1:43123/".to_string()),
            service_count: 2,
            source_path: Some("/work/blinko/install.sh".to_string()),
            source_hash: Some("blake3:source".to_string()),
        }
    }

    #[test]
    fn desktop_session_model_accepts_oci_kind() {
        let session = CapsuleSession::from_oci_snapshot(make_oci_snapshot(
            "oci-session",
            OciSessionStatus::Running,
        ));

        assert!(matches!(
            session.session_kind,
            DesktopSessionKind::Oci {
                import_kind: OciImportKind::DockerRunScript,
                service_count: 2,
                ..
            }
        ));
    }

    #[test]
    fn oci_running_session_is_visible_in_running_apps_model() {
        let mut registry = SessionRegistry::default();
        registry.sync_oci_sessions(vec![make_oci_snapshot(
            "oci-running",
            OciSessionStatus::Running,
        )]);

        let entries = registry.view_entries();

        assert!(matches!(
            entries[0].session_kind,
            DesktopSessionKind::Oci {
                status: OciSessionStatus::Running,
                ..
            }
        ));
    }

    #[test]
    fn oci_session_attached_to_window_leaves_background_rows() {
        let mut registry = SessionRegistry::default();
        registry.sync_oci_sessions(vec![make_oci_snapshot(
            "oci-running",
            OciSessionStatus::Running,
        )]);
        registry.attach_client(make_client(
            SessionClientId::next(),
            "oci-running",
            SessionClientKind::AtoWindow,
            Some(42),
            SessionClientState::Attached,
        ));

        assert!(registry.background_view_entries().is_empty());
        let entries = registry.view_entries();
        assert_eq!(entries[0].presentation_state, PresentationState::Visible);
        assert_eq!(entries[0].primary_window_id, Some(42));
    }

    #[test]
    fn oci_stop_failed_session_is_retryable() {
        let mut registry = SessionRegistry::default();
        registry.sync_oci_sessions(vec![make_oci_snapshot(
            "oci-stop-failed",
            OciSessionStatus::StopFailed,
        )]);

        let entries = registry.view_entries();

        assert!(matches!(
            entries[0].session_kind,
            DesktopSessionKind::Oci {
                status: OciSessionStatus::StopFailed,
                ..
            }
        ));
        assert_eq!(entries[0].presentation_state, PresentationState::Failed);
    }

    #[test]
    fn normal_native_source_sessions_unaffected() {
        let mut registry = SessionRegistry::default();
        registry.register_session(make_session("native", "capsule://native"));
        registry.sync_oci_sessions(vec![make_oci_snapshot("oci", OciSessionStatus::Running)]);

        let native = registry
            .view_entries()
            .into_iter()
            .find(|entry| entry.session_id == "native")
            .expect("native entry should remain");
        assert!(matches!(
            native.session_kind,
            DesktopSessionKind::NativeSource
        ));
        assert_eq!(native.handle, "capsule://native");
    }
}
