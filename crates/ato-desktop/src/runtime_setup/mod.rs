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

/// Apply the runtime-setup preferences to an in-memory config. Pure so the
/// persistence semantics are unit-testable without an `App` or disk I/O.
fn apply_runtime_setup(
    config: &mut DesktopConfig,
    podman_enabled: bool,
    node_install_enabled: bool,
    uv_install_enabled: bool,
    python_install_enabled: bool,
) {
    config.runtime.podman_enabled = podman_enabled;
    config.runtime_setup.node_install_enabled = node_install_enabled;
    config.runtime_setup.uv_install_enabled = uv_install_enabled;
    config.runtime_setup.python_install_enabled = python_install_enabled;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_runtime_setup_sets_flags() {
        let mut config = DesktopConfig::default();
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);

        apply_runtime_setup(&mut config, false, false, false, false);
        assert!(!config.runtime.podman_enabled);
        assert!(!config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(!config.runtime_setup.python_install_enabled);

        apply_runtime_setup(&mut config, true, true, false, true);
        assert!(config.runtime.podman_enabled);
        assert!(config.runtime_setup.node_install_enabled);
        assert!(!config.runtime_setup.uv_install_enabled);
        assert!(config.runtime_setup.python_install_enabled);
        // check_host_tools_on_startup is not touched by runtime setup.
        assert!(config.runtime_setup.check_host_tools_on_startup);
    }
}
