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

use crate::state::GuestRoute;
use crate::system_capsule::broker::{BrokerError, Capability};

/// Consent identity sent from the wizard JS on Approve,
/// matching the fields `approve_execution_plan_consent` expects.
#[derive(Debug, Deserialize)]
pub struct ConsentApprovalItem {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchCommand {
    /// User clicked "承認して起動" in the consent wizard.
    /// Carries the preview_id to guard against stale approvals,
    /// any new secret values to persist, non-secret config values,
    /// and execution-plan consent items to record.
    Approve {
        preview_id: String,
        #[serde(default)]
        secrets: HashMap<String, String>,
        #[serde(default)]
        config: HashMap<String, String>,
        #[serde(default)]
        consents: Vec<ConsentApprovalItem>,
    },
    /// User clicked "キャンセル" or dismissed the wizard.
    Cancel,
    /// Boot wizard's "Cancel during launch" affordance.
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
        LaunchCommand::Approve {
            preview_id,
            secrets,
            config,
            consents,
        } => {
            tracing::info!(preview_id = %preview_id, "ato_launch: user approved");

            // Warn if preview_id doesn't match the active wizard.
            let pending_preview = cx
                .try_global::<crate::window::launch_window::PendingConsentPreview>()
                .and_then(|g| g.0.clone());
            if let Some(ref preview) = pending_preview {
                if preview.preview_id != preview_id {
                    tracing::warn!(
                        expected = %preview_id,
                        current = %preview.preview_id,
                        "ato_launch: preview_id mismatch on approve"
                    );
                }
            }
            cx.set_global(crate::window::launch_window::PendingConsentPreview(None));

            let pending = cx
                .try_global::<crate::window::launch_window::PendingLaunchTarget>()
                .and_then(|g| g.0.clone());
            cx.set_global(crate::window::launch_window::PendingLaunchTarget(None));

            // Derive route handle for secret grants (must match what
            // AppCapsuleShell uses via secrets_for_capsule).
            let route_handle: Option<String> = match &pending {
                Some(GuestRoute::CapsuleHandle { handle, .. }) => Some(handle.clone()),
                Some(GuestRoute::CapsuleUrl { handle, .. }) => Some(handle.clone()),
                _ => None,
            };

            // Persist new secret values and grant them to this capsule.
            if let Some(ref handle) = route_handle {
                if !secrets.is_empty() {
                    let mut store = crate::config::load_secrets();
                    for (key, value) in &secrets {
                        if !value.is_empty() {
                            store.add_secret(key.clone(), value.clone());
                            store.grant_secret(handle, key);
                        }
                    }
                    if let Err(err) = crate::config::save_secrets(&store) {
                        tracing::error!(
                            error = %err,
                            "ato_launch: failed to save secrets — proceeding with in-memory values"
                        );
                    }
                }
            }

            // Record execution-plan consents.
            for consent in &consents {
                if let Err(err) = crate::orchestrator::approve_execution_plan_consent(
                    &consent.scoped_id,
                    &consent.version,
                    &consent.target_label,
                    &consent.policy_segment_hash,
                    &consent.provisioning_policy_hash,
                ) {
                    tracing::error!(
                        error = %err,
                        scoped_id = %consent.scoped_id,
                        "ato_launch: failed to approve consent"
                    );
                }
            }

            // Close the consent wizard so the boot wizard takes focus.
            let _ = host.update(cx, |_, window, _| window.remove_window());

            // Store non-secret config so AppCapsuleShell passes it to
            // resolve_and_start_guest.
            let plain_configs: Vec<(String, String)> = config.into_iter().collect();
            cx.set_global(crate::window::launch_window::PendingLaunchConfigs(plain_configs));

            if let Some(route) = pending {
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

                let app_handle = match crate::window::open_app_window(cx, route.clone()) {
                    Ok(h) => Some(h),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            ?route,
                            "ato_launch: open_app_window failed after approve"
                        );
                        if let Some(bh) = boot_handle {
                            let _ = bh.update(cx, |_, window, _| window.remove_window());
                        }
                        return Ok(());
                    }
                };

                if let Some(bh) = boot_handle {
                    let _ = bh.update(cx, |_, window, _| window.activate_window());
                    tracing::debug!("ato_launch: boot wizard re-activated after app window opened");
                }

                cx.set_global(crate::window::launch_window::BootWindowSlot {
                    boot_window: boot_handle,
                    app_window: app_handle,
                });

                // Record launch in the start-page history so the next
                // time the start page opens, this capsule appears in
                // the "recent capsules" row.
                if let GuestRoute::CapsuleHandle { handle, label } | GuestRoute::CapsuleUrl { handle, label, .. } = &route {
                    let mut store = crate::system_capsule::ato_start::StartPageHistoryStore::load();
                    store.record_open(handle, label);
                    if let Err(err) = store.save() {
                        tracing::warn!(error = %err, "ato_launch: failed to save start history");
                    }
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
            cx.set_global(crate::window::launch_window::PendingConsentPreview(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        LaunchCommand::AbortBoot => {
            tracing::info!("ato_launch: user aborted boot — closing both windows");

            let slot = cx
                .try_global::<crate::window::launch_window::BootWindowSlot>()
                .cloned()
                .unwrap_or_default();
            cx.set_global(crate::window::launch_window::BootWindowSlot::default());

            if let Some(boot) = slot.boot_window {
                let _ = boot.update(cx, |_, window, _| window.remove_window());
            }
            if let Some(app) = slot.app_window {
                let _ = app.update(cx, |_, window, _| window.remove_window());
            }
        }
    }
    Ok(())
}
