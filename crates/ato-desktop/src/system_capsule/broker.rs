//! `CapabilityBroker` — the single chokepoint that holds `&mut App`
//! privileged operations on behalf of system capsules.
//!
//! Stage A scope: validate capability allowlist, dispatch to the
//! correct per-capsule module. The per-capsule modules execute the
//! same operations the old `card_switcher::dispatch` /
//! `start_window::dispatch` functions already perform; this commit
//! just funnels them behind a typed enum + allowlist check.
//!
//! Phase 2 will add provenance validation (originating URL matches
//! `capsule://system/<expected>`, user-gesture sentinel, etc.). The
//! match arm marked `Phase 2 hook` is the insertion point.

use gpui::{AnyWindowHandle, App};

use super::{
    ato_identity, ato_launch, ato_settings, ato_start, ato_store, ato_web_viewer, ato_windows,
    manifest,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SystemCapsuleId {
    AtoStore,
    AtoWebViewer,
    AtoSettings,
    AtoWindows,
    /// Pre-flight consent wizard + mid-flight boot progress for
    /// capsule launches. Two HTML views inside the same capsule:
    /// `assets/system/ato-launch/{consent,boot}.html`.
    AtoLaunch,
    /// Account / Identity popover shown when the user clicks the
    /// avatar at the right of the Control Bar. `ato-identity/index.html`.
    AtoIdentity,
    /// "New window" start page. `ato-start/index.html`.
    AtoStart,
}

/// Vocabulary of system-capability tokens. Each per-capsule command
/// declares the capability it needs via
/// `SystemCommand::required_capability()`; the broker checks the
/// manifest's `allowed_capabilities` list before invoking the
/// per-capsule dispatcher.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Capability {
    /// Open a new content WebView (i.e. spawn a new AppWindow).
    WebviewCreate,
    /// Open a new tab inside the calling capsule's surface
    /// (currently only the `ato-web-viewer` browser uses this).
    TabsCreate,
    /// Read `OpenContentWindows` to list / snapshot windows.
    WindowsList,
    /// Raise a tracked window to the foreground.
    WindowsActivate,
    /// Close the calling capsule's own host window.
    WindowsClose,
    /// Read desktop settings.
    SettingsRead,
    /// Mutate desktop settings. Gated as `Forbidden` in Phase 1 even
    /// for `ato-settings`; Phase 2 will open this behind a consent
    /// prompt.
    SettingsWrite,
    /// Open another system capsule (e.g. `ato-windows` asking the
    /// broker to launch `ato-store`).
    LaunchSystemCapsule,
}

/// Typed envelope: every privileged request from a system capsule
/// must be expressible as one of these. The IPC layer
/// (`super::ipc`, added in Stage B) deserialises the
/// `{capsule, command}` JSON into this enum.
#[derive(Debug)]
pub enum SystemCommand {
    AtoWindows(ato_windows::WindowsCommand),
    AtoStore(ato_store::StoreCommand),
    AtoSettings(ato_settings::SettingsCommand),
    AtoWebViewer(ato_web_viewer::WebViewerCommand),
    AtoLaunch(ato_launch::LaunchCommand),
    AtoIdentity(ato_identity::IdentityCommand),
    AtoStart(ato_start::AtoStartCommand),
}

impl SystemCommand {
    /// The capability this command requires. Used by the broker to
    /// gate against the calling capsule's manifest.
    pub fn required_capability(&self) -> Capability {
        match self {
            SystemCommand::AtoWindows(c) => c.required_capability(),
            SystemCommand::AtoStore(c) => c.required_capability(),
            SystemCommand::AtoSettings(c) => c.required_capability(),
            SystemCommand::AtoWebViewer(c) => c.required_capability(),
            SystemCommand::AtoLaunch(c) => c.required_capability(),
            SystemCommand::AtoIdentity(c) => c.required_capability(),
            SystemCommand::AtoStart(c) => c.required_capability(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("capsule {capsule:?} is not allowed capability {capability:?}")]
    Forbidden {
        capsule: SystemCapsuleId,
        capability: Capability,
    },
    /// Reserved for Phase 2 (provenance failure, malformed payload).
    #[error("system capsule broker error: {0}")]
    Internal(String),
}

pub struct CapabilityBroker;

impl CapabilityBroker {
    /// Dispatch a typed system command on behalf of the given
    /// capsule. Validates the capability allowlist before invoking
    /// the per-capsule handler.
    ///
    /// `host` is the GPUI window handle that hosted the request —
    /// the per-capsule handler uses it to close its own window when
    /// the command is `Close`.
    pub fn dispatch(
        cx: &mut App,
        host: AnyWindowHandle,
        capsule: SystemCapsuleId,
        command: SystemCommand,
    ) -> Result<(), BrokerError> {
        let manifest = manifest::lookup(capsule);
        let required = command.required_capability();
        if !manifest.allowed_capabilities.contains(&required) {
            tracing::warn!(
                ?capsule,
                capability = ?required,
                "broker: Forbidden — capsule not allowed this capability"
            );
            return Err(BrokerError::Forbidden {
                capsule,
                capability: required,
            });
        }

        // Phase 2 hook: provenance check
        //   - verify the IPC originated from the expected
        //     `capsule://system/<slug>` URL
        //   - if the command requires a user gesture, verify the
        //     gesture sentinel set by the preload script
        //
        // Phase 1 trusts the IPC boundary alone (only system-capsule
        // pages can post here once the protocol routing in Stage B
        // lands).

        tracing::debug!(?capsule, ?required, "broker: dispatch");
        match command {
            SystemCommand::AtoWindows(c) => ato_windows::dispatch(cx, host, c),
            SystemCommand::AtoStore(c) => ato_store::dispatch(cx, host, c),
            SystemCommand::AtoSettings(c) => ato_settings::dispatch(cx, host, c),
            SystemCommand::AtoWebViewer(c) => ato_web_viewer::dispatch(cx, host, c),
            SystemCommand::AtoLaunch(c) => ato_launch::dispatch(cx, host, c),
            SystemCommand::AtoIdentity(c) => ato_identity::dispatch(cx, host, c),
            SystemCommand::AtoStart(c) => ato_start::dispatch(cx, host, c),
        }
    }
}
