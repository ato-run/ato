//! `ato-launch` system capsule — capsule-launch wizards.
//!
//! Two HTML views:
//!   - `assets/system/ato-launch/consent.html` — pre-flight
//!     consent wizard. Shows the capsule's identity, requested
//!     permissions, and any required env-var inputs. User clicks
//!     "承認して起動" or "キャンセル".
//!   - `assets/system/ato-launch/boot.html` — mid-flight boot
//!     progress. Shows the launch steps (Capsule取得 → 依存解決
//!     → 起動環境 → セキュリティ → データ保護 → プライバシー).
//!
//! Phase 1 ships both views as standalone demonstrable shells —
//! they are openable via MCP for AODD, but are NOT yet hooked into
//! the real `crate::orchestrator::resolve_and_start_guest` capsule
//! launch flow. Phase 2 will (a) gate every CapsuleHandle spawn on
//! a consent decision and (b) drive boot progress from orchestrator
//! events.
//!
//! Phase 1 dispatch handlers close the wizard window on
//! Approve/Cancel and log the outcome. Approve carries the capsule
//! handle so a follow-up iteration can spawn the AppWindow once the
//! consent flow is real.

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchCommand {
    /// User clicked "承認して起動" in the consent wizard. Carries
    /// the capsule handle so Phase 2 can spawn the AppWindow.
    Approve { handle: String },
    /// User clicked "キャンセル" or dismissed the wizard.
    Cancel,
    /// Boot wizard's "Cancel during launch" affordance. Identical
    /// effect to Cancel for Phase 1; Phase 2 will signal the
    /// orchestrator to abort the in-flight session.
    AbortBoot,
}

impl LaunchCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            LaunchCommand::Approve { .. } => Capability::WebviewCreate,
            LaunchCommand::Cancel | LaunchCommand::AbortBoot => Capability::WindowsClose,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: LaunchCommand,
) -> Result<(), BrokerError> {
    match command {
        LaunchCommand::Approve { handle } => {
            tracing::info!(
                target_handle = %handle,
                "ato_launch: user approved — Phase 2 will spawn the AppWindow here"
            );
            // Phase 1: just close the wizard window. Phase 2 spawns
            // `open_app_window(GuestRoute::CapsuleHandle { handle, label })`.
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        LaunchCommand::Cancel => {
            tracing::info!("ato_launch: user cancelled");
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        LaunchCommand::AbortBoot => {
            tracing::info!(
                "ato_launch: user aborted boot — Phase 2 will signal the orchestrator to stop the in-flight session"
            );
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
}
