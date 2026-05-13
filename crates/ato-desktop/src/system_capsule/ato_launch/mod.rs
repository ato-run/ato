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

use std::collections::HashMap;

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::system_capsule::broker::{BrokerError, Capability};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchCommand {
    /// User clicked "承認して起動" in the consent wizard. Carries
    /// the capsule handle and any env-var config values collected
    /// from the form so the launch thread can pass them to
    /// `resolve_and_start_guest` without surfacing a MissingConfig error.
    Approve {
        handle: String,
        #[serde(default)]
        config: HashMap<String, String>,
    },
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
        LaunchCommand::Approve { handle, config } => {
            tracing::info!(target_handle = %handle, "ato_launch: user approved");
            // Close the consent wizard so the boot wizard takes focus.
            let _ = host.update(cx, |_, window, _| window.remove_window());

            let pending = cx
                .try_global::<crate::window::launch_window::PendingLaunchTarget>()
                .and_then(|g| g.0.clone());
            cx.set_global(crate::window::launch_window::PendingLaunchTarget(None));

            // Store the config values so `open_app_window` → `AppCapsuleShell::new`
            // can read them and pass to `resolve_and_start_guest`.
            let plain_configs: Vec<(String, String)> = config.into_iter().collect();
            cx.set_global(crate::window::launch_window::PendingLaunchConfigs(plain_configs));

            if let Some(route) = pending {
                // Open boot wizard FIRST so there is visible progress
                // feedback before the background thread even starts.
                let boot_handle =
                    match crate::window::launch_window::open_boot_window(cx, Some(&route)) {
                        Ok(h) => Some(h),
                        Err(err) => {
                            tracing::error!(
                                error = %err,
                                "ato_launch: open_boot_window failed after approve"
                            );
                            None
                        }
                    };

                // Open the destination AppWindow SECOND. AppCapsuleShell
                // is created inside and immediately starts the background
                // launch thread.
                let app_handle = match crate::window::open_app_window(cx, route.clone()) {
                    Ok(h) => Some(h),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            ?route,
                            "ato_launch: open_app_window failed after approve"
                        );
                        // Boot wizard is already open with no app to back it;
                        // close it immediately to avoid an orphaned overlay.
                        if let Some(bh) = boot_handle {
                            let _ = bh.update(cx, |_, window, _| window.remove_window());
                        }
                        return Ok(());
                    }
                };

                // Re-activate the boot wizard: open_app_window opens with
                // focus: true, which steals focus and puts the app window on
                // top of the boot wizard. The boot wizard must be the dominant
                // UI during the boot phase.
                if let Some(bh) = boot_handle {
                    let _ = bh.update(cx, |_, window, _| window.activate_window());
                    tracing::debug!("ato_launch: boot wizard re-activated after app window opened");
                }

                // Register both handles in the global slot so AbortBoot and
                // AppCapsuleShell's polling task can close them.
                //
                // GPUI is single-threaded here, so this set is guaranteed
                // to happen before the polling task's first tick.
                cx.set_global(crate::window::launch_window::BootWindowSlot {
                    boot_window: boot_handle,
                    app_window: app_handle,
                });
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
            tracing::info!("ato_launch: user aborted boot — closing both windows");

            // Read and clear the slot atomically within this GPUI turn.
            let slot = cx
                .try_global::<crate::window::launch_window::BootWindowSlot>()
                .cloned()
                .unwrap_or_default();
            cx.set_global(crate::window::launch_window::BootWindowSlot::default());

            // Close the boot wizard (may be `host` itself or a sibling).
            if let Some(boot) = slot.boot_window {
                let _ = boot.update(cx, |_, window, _| window.remove_window());
            }
            // Close the AppWindow. GPUI drops AppCapsuleShell → its Drop
            // sets abort_flag and stops any running session.
            if let Some(app) = slot.app_window {
                let _ = app.update(cx, |_, window, _| window.remove_window());
            }
        }
    }
    Ok(())
}
