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
            tracing::info!(target_handle = %handle, "ato_launch: user approved");
            // Close the consent wizard first so the AppWindow and boot
            // wizard take focus on top of a clean stack.
            let _ = host.update(cx, |_, window, _| window.remove_window());

            // Consume the pending launch target. If the wizard was
            // opened via the real NavigateToUrl path, this holds the
            // GuestRoute to spawn. If the wizard was opened standalone
            // via MCP for AODD, there is no pending target — we log
            // and skip the AppWindow spawn (honest no-op).
            let pending = cx
                .try_global::<crate::window::launch_window::PendingLaunchTarget>()
                .and_then(|g| g.0.clone());
            cx.set_global(crate::window::launch_window::PendingLaunchTarget(None));

            if let Some(route) = pending {
                if let Err(err) = crate::window::open_app_window(cx, route.clone()) {
                    tracing::error!(error = %err, ?route, "ato_launch: open_app_window failed after approve");
                }
                // Launch-ceremony overlay. The boot wizard's step
                // animation is decorative (Phase 1). Phase 2 will drive
                // it from real orchestrator events and auto-close on
                // "ready".
                if let Err(err) = crate::window::launch_window::open_boot_window(cx) {
                    tracing::error!(error = %err, "ato_launch: open_boot_window failed after approve");
                }
            } else {
                tracing::info!(
                    "ato_launch: approve from MCP/standalone (no pending target) — wizard closed, no AppWindow spawned"
                );
            }
        }
        LaunchCommand::Cancel => {
            tracing::info!("ato_launch: user cancelled");
            cx.set_global(crate::window::launch_window::PendingLaunchTarget(None));
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
