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
    },
    /// Persist the runtime-safety opt-out toggles collected on the
    /// runtime-safety onboarding step. Sent before `Complete` so the
    /// choices land in desktop config regardless of how the flow finishes.
    /// Both settings default on (opt-out), so a missing field is treated as
    /// enabled.
    SaveRuntimeOptoutSettings {
        #[serde(default = "default_true")]
        podman_enabled: bool,
        #[serde(default = "default_true")]
        host_device_detection_enabled: bool,
    },
}

fn default_true() -> bool {
    true
}

impl OnboardingCommand {
    pub fn required_capability(&self) -> Capability {
        // Both onboarding commands are scoped to the onboarding capsule and
        // gated by the same first-run capability. Persisting the opt-out
        // toggles is part of completing onboarding, so it reuses
        // `OnboardingComplete` rather than introducing a second token.
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
        OnboardingCommand::Complete { version, skipped } => {
            let mut config = load_config();
            config.desktop.onboarding.completed = true;
            config.desktop.onboarding.skipped = skipped;
            config.desktop.onboarding.version = version.max(ONBOARDING_VERSION);
            let startup_surface = config.desktop.startup_surface;
            save_config(&config);

            let _ = host.update(cx, |_, window, _| window.remove_window());

            crate::window::open_configured_startup_surface(cx, startup_surface)
                .map_err(|err| BrokerError::Internal(err.to_string()))?;
        }
        OnboardingCommand::SaveRuntimeOptoutSettings {
            podman_enabled,
            host_device_detection_enabled,
        } => {
            // Persist only — do not close the window or open the startup
            // surface. The onboarding page sends this immediately before the
            // terminal `Complete` command, which owns the window teardown.
            let mut config = load_config();
            apply_runtime_optout(&mut config, podman_enabled, host_device_detection_enabled);
            save_config(&config);
        }
    }

    Ok(())
}

/// Apply the runtime-safety opt-out toggles to an in-memory config. Pure so
/// the persistence semantics are unit-testable without an `App` or disk I/O.
fn apply_runtime_optout(
    config: &mut DesktopConfig,
    podman_enabled: bool,
    host_device_detection_enabled: bool,
) {
    config.runtime.podman_enabled = podman_enabled;
    config.privacy.host_device_detection_enabled = host_device_detection_enabled;
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
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn save_runtime_optout_parses_disabled_values() {
        let json = r#"{
            "kind": "save_runtime_optout_settings",
            "podman_enabled": false,
            "host_device_detection_enabled": false
        }"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeOptoutSettings {
                podman_enabled,
                host_device_detection_enabled,
            } => {
                assert!(!podman_enabled);
                assert!(!host_device_detection_enabled);
            }
            other => panic!("expected SaveRuntimeOptoutSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_optout_defaults_missing_fields_to_enabled() {
        let json = r#"{"kind": "save_runtime_optout_settings"}"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeOptoutSettings {
                podman_enabled,
                host_device_detection_enabled,
            } => {
                assert!(podman_enabled);
                assert!(host_device_detection_enabled);
            }
            other => panic!("expected SaveRuntimeOptoutSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_optout_requires_onboarding_capability() {
        let cmd = OnboardingCommand::SaveRuntimeOptoutSettings {
            podman_enabled: false,
            host_device_detection_enabled: false,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

    #[test]
    fn apply_runtime_optout_sets_both_flags() {
        let mut config = DesktopConfig::default();
        assert!(config.runtime.podman_enabled);
        assert!(config.privacy.host_device_detection_enabled);

        apply_runtime_optout(&mut config, false, false);
        assert!(!config.runtime.podman_enabled);
        assert!(!config.privacy.host_device_detection_enabled);

        apply_runtime_optout(&mut config, true, false);
        assert!(config.runtime.podman_enabled);
        assert!(!config.privacy.host_device_detection_enabled);
    }
}
