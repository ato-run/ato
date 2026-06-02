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
    /// Persist the host runtime-setup preferences collected on the Runtime
    /// Setup onboarding step (issue #420 revision). Sent before `Complete` so
    /// the choices land in desktop config regardless of how the flow finishes.
    /// Every field defaults on (opt-out), so a missing field is treated as
    /// enabled — the keyboard "finish" path and the button submit the same set.
    SaveRuntimeSetupSettings {
        /// Whether Podman may be used as an OCI provider (`runtime.podman_enabled`).
        #[serde(default = "default_true")]
        podman_enabled: bool,
        /// Whether Ato may install an Ato-managed Node when a recipe needs it.
        #[serde(default = "default_true")]
        node_install_enabled: bool,
        /// Whether Ato may install an Ato-managed uv when a recipe needs it.
        #[serde(default = "default_true")]
        uv_install_enabled: bool,
        /// Whether Ato may install an Ato-managed Python when a recipe needs it.
        #[serde(default = "default_true")]
        python_install_enabled: bool,
    },
}

fn default_true() -> bool {
    true
}

impl OnboardingCommand {
    pub fn required_capability(&self) -> Capability {
        // Both onboarding commands are scoped to the onboarding capsule and
        // gated by the same first-run capability. Persisting the runtime-setup
        // preferences is part of completing onboarding, so it reuses
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
        OnboardingCommand::SaveRuntimeSetupSettings {
            podman_enabled,
            node_install_enabled,
            uv_install_enabled,
            python_install_enabled,
        } => {
            // Persist only — do not close the window or open the startup
            // surface. The onboarding page sends this immediately before the
            // terminal `Complete` command, which owns the window teardown.
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
    }

    Ok(())
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
    fn save_runtime_setup_parses_disabled_values() {
        let json = r#"{
            "kind": "save_runtime_setup_settings",
            "podman_enabled": false,
            "node_install_enabled": false,
            "uv_install_enabled": false,
            "python_install_enabled": false
        }"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeSetupSettings {
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            } => {
                assert!(!podman_enabled);
                assert!(!node_install_enabled);
                assert!(!uv_install_enabled);
                assert!(!python_install_enabled);
            }
            other => panic!("expected SaveRuntimeSetupSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_setup_defaults_missing_fields_to_enabled() {
        let json = r#"{"kind": "save_runtime_setup_settings"}"#;
        let cmd: OnboardingCommand = serde_json::from_str(json).unwrap();
        match cmd {
            OnboardingCommand::SaveRuntimeSetupSettings {
                podman_enabled,
                node_install_enabled,
                uv_install_enabled,
                python_install_enabled,
            } => {
                assert!(podman_enabled);
                assert!(node_install_enabled);
                assert!(uv_install_enabled);
                assert!(python_install_enabled);
            }
            other => panic!("expected SaveRuntimeSetupSettings, got {other:?}"),
        }
    }

    #[test]
    fn save_runtime_setup_requires_onboarding_capability() {
        let cmd = OnboardingCommand::SaveRuntimeSetupSettings {
            podman_enabled: false,
            node_install_enabled: false,
            uv_install_enabled: false,
            python_install_enabled: false,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }

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
        // check_host_tools_on_startup is not touched by onboarding.
        assert!(config.runtime_setup.check_host_tools_on_startup);
    }
}
