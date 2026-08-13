//! `ato-onboarding` system capsule — first-run onboarding flow.
//!
//! Scope (issue #420 architecture correction): this module owns *only*
//! onboarding-specific concerns — completing/skipping the flow and advancing to
//! the configured startup surface. The Runtime Setup panel shown on the last
//! onboarding step is backed by the shared [`crate::runtime_setup`] feature
//! module: those commands (`runtime_setup_status`, `install_runtime_tools`,
//! `save_runtime_setup_settings`, …) are routed by command `kind` in the IPC
//! parser, not handled here, so the Settings surface can reuse them verbatim.

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::config::{DesktopConfig, load_config, save_config};
use crate::system_capsule::broker::{BrokerError, Capability};

pub const ONBOARDING_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnboardingCommand {
    Complete {
        version: u16,
        #[serde(default)]
        skipped: bool,
        /// #460 PR3: when set, resume straight into this capsule launch after
        /// onboarding closes (the "Continue to sample app" path), instead of the
        /// configured startup surface. A `capsule://…` handle / URL.
        #[serde(default)]
        launch_handle: Option<String>,
    },
}

impl OnboardingCommand {
    pub fn required_capability(&self) -> Capability {
        // Completing onboarding is the one onboarding-only privileged action.
        // Runtime-setup reads/installs are NOT routed here — they use the
        // feature-level `RuntimeSetup*` capabilities via `crate::runtime_setup`.
        Capability::OnboardingComplete
    }
}

pub fn should_show_onboarding(config: &DesktopConfig) -> bool {
    !config.desktop.onboarding.completed && config.desktop.onboarding.version < ONBOARDING_VERSION
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: OnboardingCommand,
) -> Result<(), BrokerError> {
    match command {
        OnboardingCommand::Complete {
            version,
            skipped,
            launch_handle,
        } => {
            // Tear down any in-flight runtime install before leaving the flow.
            crate::runtime_setup::cancel_active_install(cx);
            let mut config = load_config();
            config.desktop.onboarding.completed = true;
            config.desktop.onboarding.skipped = skipped;
            config.desktop.onboarding.version = version.max(ONBOARDING_VERSION);
            let startup_surface = config.desktop.startup_surface;
            save_config(&config);

            let _ = host.update(cx, |_, window, _| window.remove_window());

            // #460 PR3 (Case A): if onboarding finished with a "Continue to
            // sample app" intent, resume straight into that capsule launch via
            // the existing consent/launch path — no shell, no manual return.
            // Otherwise land on the configured startup surface.
            match launch_handle.filter(|h| !h.trim().is_empty()) {
                Some(handle) => {
                    let route = crate::state::GuestRoute::CapsuleHandle {
                        handle: handle.clone(),
                        label: handle.clone(),
                        community_toml_id: None,
                    };
                    crate::window::launch_window::open_consent_window_for_route(cx, route)
                        .map_err(|err| BrokerError::Internal(err.to_string()))?;
                }
                None => {
                    crate::window::open_configured_startup_surface(cx, startup_surface)
                        .map_err(|err| BrokerError::Internal(err.to_string()))?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DesktopConfig;
    use crate::system_capsule::broker::Capability;

    #[test]
    fn should_show_onboarding_for_default_config() {
        assert!(should_show_onboarding(&DesktopConfig::default()));
    }

    #[test]
    fn should_not_show_when_completed() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn skipped_and_completed_stays_hidden() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.skipped = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn complete_requires_onboarding_capability() {
        let cmd = OnboardingCommand::Complete {
            version: ONBOARDING_VERSION,
            skipped: false,
            launch_handle: None,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn complete_without_launch_handle_defaults_to_none() {
        // Existing clients that omit `launch_handle` must still deserialize.
        let cmd: OnboardingCommand =
            serde_json::from_str(r#"{"kind":"complete","version":1,"skipped":false}"#).unwrap();
        match cmd {
            OnboardingCommand::Complete { launch_handle, .. } => assert_eq!(launch_handle, None),
        }
    }

    #[test]
    fn complete_carries_launch_handle() {
        // #460 PR3: "Continue to sample app" sends a capsule handle.
        let cmd: OnboardingCommand = serde_json::from_str(
            r#"{"kind":"complete","version":1,"launch_handle":"capsule://github.com/sosedoff/pgweb"}"#,
        )
        .unwrap();
        match cmd {
            OnboardingCommand::Complete { launch_handle, .. } => {
                assert_eq!(
                    launch_handle.as_deref(),
                    Some("capsule://github.com/sosedoff/pgweb")
                );
            }
        }
    }
}
