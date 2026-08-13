//! Runtime Setup feature module (issue #420).
//!
//! Shared backend for the Runtime Setup surface used by BOTH the first-run
//! onboarding flow (`ato-onboarding`) and the post-onboarding Settings surface
//! (`ato-settings`). The detection / install *logic* lives in the bundled
//! `ato` helper (`ato internal runtime …`); this module owns the desktop-side
//! IPC orchestration: shelling out, streaming progress into whichever surface
//! is open, and persisting opt-out preferences.
//!
//! Organised by feature, not by frontend app. Both frontends call the same
//! feature-level IPC commands ([`RuntimeSetupCommand`]) and are gated by
//! feature-level capabilities (`RuntimeSetupRead` / `RuntimeSetupInstall` /
//! `RuntimeSetupOpenLogs`) — never by `OnboardingComplete`. The capability a
//! command needs is fixed; what differs is which capsule's manifest grants it.

mod install;
pub(crate) mod launch_intent;
mod prepare;
mod status;
mod types;

pub use types::RuntimeSetupCommand;

use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui::{AnyWindowHandle, App};
use serde_json::Value;

use crate::config::{DesktopConfig, load_config, save_config};
use crate::system_capsule::broker::BrokerError;

/// A foreground runtime install in progress. Shared (via [`ActiveRuntimeInstall`])
/// so the explicit cancel command and onboarding's `Complete` teardown can both
/// reach the running child.
#[derive(Clone)]
pub(crate) struct RuntimeInstallJob {
    cancel_requested: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl RuntimeInstallJob {
    fn new() -> Self {
        Self {
            cancel_requested: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        }
    }

    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
    }
}

#[derive(Default)]
pub(crate) struct ActiveRuntimeInstall(Option<RuntimeInstallJob>);

impl gpui::Global for ActiveRuntimeInstall {}

/// Dispatch a Runtime Setup command. `_host` is accepted to match the broker
/// per-capsule dispatch signature; the active surface is resolved via the
/// onboarding/settings shell globals so a streamed install can keep hydrating
/// after the originating IPC has unwound.
pub fn dispatch(
    cx: &mut App,
    _host: AnyWindowHandle,
    command: RuntimeSetupCommand,
) -> Result<(), BrokerError> {
    match command {
        RuntimeSetupCommand::RuntimeSetupStatus { request_id } => {
            status::spawn_runtime_setup_status(cx, request_id);
        }
        RuntimeSetupCommand::SaveRuntimeSetupSettings {
            podman_enabled,
            node_install_enabled,
            uv_install_enabled,
            python_install_enabled,
        } => {
            // Persist only — never close a window or open a surface. The
            // onboarding page sends this immediately before its terminal
            // `Complete` command (which owns window teardown); the settings
            // page sends it as a standalone preference write.
            let mut config = load_config();
            apply_runtime_setup(
                &mut config,
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            );
            save_config(&config);
        }
        RuntimeSetupCommand::InstallRuntimeTools { request_id, tools } => {
            install::start_runtime_install(cx, request_id, tools);
        }
        RuntimeSetupCommand::PrepareRuntimeTools { request_id, tools } => {
            prepare::start_runtime_prepare(cx, request_id, tools);
        }
        RuntimeSetupCommand::CancelRuntimeInstall { request_id } => {
            let cancelled = cancel_active_install(cx);
            let response = serde_json::json!({
                "ok": cancelled,
                "requestId": request_id,
                "runtimeInstallCancelled": cancelled,
                "error": if cancelled { Value::Null } else { serde_json::json!({ "message": "no runtime install is active" }) },
            });
            push_runtime_setup(cx, &response.to_string());
        }
        RuntimeSetupCommand::OpenRuntimeSetupLogs { request_id } => {
            install::open_runtime_setup_logs(cx, request_id);
        }
        RuntimeSetupCommand::PrepareWindowsRuntimeSubstrate {
            request_id,
            action,
            source_surface,
        } => {
            prepare::start_windows_substrate(cx, request_id, action, source_surface);
        }
        RuntimeSetupCommand::RepairHostRuntime { request_id } => {
            prepare::start_runtime_repair(cx, request_id);
        }
        RuntimeSetupCommand::ResumeRuntimeSetupAfterReboot { request_id } => {
            status::spawn_runtime_setup_resume(cx, request_id);
        }
        RuntimeSetupCommand::CancelPendingLaunch { request_id } => {
            // Clear the interrupted-launch marker only; Runtime Setup itself is
            // untouched. Then clear the banner on the active surface(s).
            launch_intent::clear_pending_launch();
            launch_intent::push_pending_launch(cx, None);
            let response = serde_json::json!({
                "ok": true,
                "requestId": request_id,
                "pendingLaunchCancelled": true,
            });
            push_runtime_setup(cx, &response.to_string());
        }
    }
    Ok(())
}

fn ensure_install_global(cx: &mut App) {
    if cx.try_global::<ActiveRuntimeInstall>().is_none() {
        cx.set_global(ActiveRuntimeInstall::default());
    }
}

/// Cancel the active install if one is running. Returns whether a job was
/// cancelled. Safe to call when nothing is running.
pub(crate) fn cancel_active_install(cx: &mut App) -> bool {
    ensure_install_global(cx);
    let Some(job) = cx.global_mut::<ActiveRuntimeInstall>().0.take() else {
        return false;
    };
    job.cancel();
    true
}

/// Forward a payload to whichever Runtime Setup surface is open. Each surface
/// hydrates through its own JS hook and filters on the `runtimeSetup*` /
/// `runtimeInstall*` fields, so delivering to both (when, rarely, both are
/// open) is harmless.
fn push_runtime_setup(cx: &mut App, payload_json: &str) {
    crate::window::onboarding_window::hydrate_active_runtime_setup(cx, payload_json);
    crate::window::settings_window::hydrate_active_runtime_setup(cx, payload_json);
}

/// Apply runtime-setup preferences to an in-memory config. Each argument is
/// optional: `None` preserves the stored value, so a caller (e.g. the Settings
/// Runtime tab) can persist a subset without resetting the others. Pure so the
/// semantics are unit-testable without an `App` or disk I/O.
fn apply_runtime_setup(
    config: &mut DesktopConfig,
    podman_enabled: Option<bool>,
    node_install_enabled: Option<bool>,
    uv_install_enabled: Option<bool>,
    python_install_enabled: Option<bool>,
) {
    if let Some(v) = podman_enabled {
        config.runtime.podman_enabled = v;
    }
    if let Some(v) = node_install_enabled {
        config.runtime_setup.node_install_enabled = v;
    }
    if let Some(v) = uv_install_enabled {
        config.runtime_setup.uv_install_enabled = v;
    }
    if let Some(v) = python_install_enabled {
        config.runtime_setup.python_install_enabled = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_runtime_setup_sets_flags() {
        let mut config = DesktopConfig::default();
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);

        apply_runtime_setup(
            &mut config,
            Some(false),
            Some(false),
            Some(false),
            Some(false),
        );
        assert!(!config.runtime.podman_enabled);
        assert!(!config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(!config.runtime_setup.python_install_enabled);

        apply_runtime_setup(&mut config, Some(true), Some(true), Some(false), Some(true));
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(config.runtime_setup.python_install_enabled);
        // check_host_tools_on_startup is not touched by runtime setup.
        assert!(config.runtime_setup.check_host_tools_on_startup);
    }

    #[test]
    fn apply_runtime_setup_preserves_omitted_fields() {
        // The Settings Runtime tab persists Node/uv/Python but omits Podman:
        // a None field must leave the stored value untouched (no silent reset).
        let mut config = DesktopConfig::default();
        config.runtime.podman_enabled = false; // user opted out during onboarding

        apply_runtime_setup(&mut config, None, Some(true), None, None);

        assert!(
            !config.runtime.podman_enabled,
            "omitted podman_enabled must be preserved, not reset to default"
        );
        assert!(config.runtime_setup.node_install_enabled);
    }
}
